// Writes the 2.x update manifest (`latest-v2.json`) that kwikpaste-updater reads.
//
// Same shape as the Tauri static manifest 1.x used (version, notes, pub_date, platforms → { url,
// signature }) with the same 10 platform keys, plus the `kwikpaste` block: `schema: 1` and the minimum
// OS versions, so an updater never installs a build the system cannot run (the portable zip and the
// macOS bundle have no installer to check that).
//
// On GitHub the file is always called latest-v2.json, never latest.json: old clients fall back to
// releases/latest/download/latest.json. Only the Qiniu mirror publishes it as kwikpaste/v2/<channel>/latest.json.
//
// Every platform must be present, every signature must verify against --pubkey with the local file and
// name it (`file:<asset name>` in the trusted comment, which kwikpaste-updater checks), and
// with --assets (the draft release's asset list) every file must be uploaded with the local size.
// URLs are always --base-url/<name>: a draft's browser_download_url points at releases/download/untagged-…,
// which stops resolving once the draft is published.
//
// Usage:
//   node scripts/release/v2-manifest.mjs --version <v> --dir <assets dir> --pubkey <file> --out latest-v2.json
//     --base-url <url> [--assets <assets.json>] [--notes <file>] [--pub-date <RFC 3339>]
//   node scripts/release/v2-manifest.mjs --sample [--out <file> | --check <file>]
//     The deterministic sample kwikpaste-updater parses in its tests
//     (crates/kwikpaste-updater/fixtures/latest-v2.sample.json); --check compares it with the file.
import { existsSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

import { parsePublicKey, verifySignature } from "../ci/lib/minisign.mjs";

export const REQUIRES = { macos: "10.15", windowsBuild: 17134 };

/**
 * Platform key → asset name, for the 1.x keys plus the portable ones.
 */
export const platformAssets = (version) => {
  const setup = (arch) => {
    return `KwikPaste_${version}_${arch}-setup.exe`;
  };
  const portable = (arch) => {
    return `KwikPaste_${version}_${arch}_portable.zip`;
  };
  const app = (arch) => {
    return `KwikPaste_${version}_${arch}.app.tar.gz`;
  };

  return {
    "darwin-aarch64": app("aarch64"),
    "darwin-aarch64-app": app("aarch64"),
    "darwin-x86_64": app("x64"),
    "darwin-x86_64-app": app("x64"),
    "windows-aarch64": setup("arm64"),
    "windows-aarch64-nsis": setup("arm64"),
    "windows-aarch64-portable": portable("arm64"),
    "windows-x86_64": setup("x64"),
    "windows-x86_64-nsis": setup("x64"),
    "windows-x86_64-portable": portable("x64"),
  };
};

/**
 * Builds the manifest object. `urlFor(name)` and `signatureFor(name)` resolve each asset.
 */
export const buildManifest = ({
  version,
  notes,
  pubDate,
  urlFor,
  signatureFor,
}) => {
  const platforms = {};
  for (const [key, name] of Object.entries(platformAssets(version))) {
    platforms[key] = { signature: signatureFor(name), url: urlFor(name) };
  }

  return {
    kwikpaste: { requires: REQUIRES, schema: 1 },
    notes,
    platforms,
    pub_date: pubDate,
    version,
  };
};

const sample = () => {
  return buildManifest({
    notes: "Sample notes",
    pubDate: "2027-04-05T08:00:00Z",
    signatureFor: (name) => {
      return Buffer.from(`signature of ${name}`).toString("base64");
    },
    urlFor: (name) => {
      return `https://github.com/ManSanDADADA/KwikPaste/releases/download/v2.0.0/${name}`;
    },
    version: "2.0.0",
  });
};

const serialize = (manifest) => {
  return `${JSON.stringify(manifest, null, 2)}\n`;
};

const parseArgs = (argv) => {
  const options = {};
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    const next = () => {
      index += 1;
      return argv[index];
    };
    if (arg === "--sample") {
      options.sample = true;
    } else if (arg === "--check") {
      options.check = next();
    } else if (arg === "--version") {
      options.version = next();
    } else if (arg === "--dir") {
      options.dir = next();
    } else if (arg === "--pubkey") {
      options.pubkey = next();
    } else if (arg === "--out") {
      options.out = next();
    } else if (arg === "--assets") {
      options.assets = next();
    } else if (arg === "--base-url") {
      options.baseUrl = next().replace(/\/$/, "");
    } else if (arg === "--notes") {
      options.notes = next();
    } else if (arg === "--pub-date") {
      options.pubDate = next();
    } else {
      throw new Error(`unknown argument ${arg}`);
    }
  }

  return options;
};

const main = () => {
  const options = parseArgs(process.argv.slice(2));
  if (options.sample) {
    const text = serialize(sample());
    if (options.check) {
      if (
        readFileSync(options.check, "utf8").replaceAll("\r\n", "\n") !== text
      ) {
        throw new Error(
          `${options.check} is not the current sample; regenerate it with --sample`,
        );
      }
      process.stdout.write(`ok  ${options.check} matches the generator\n`);
      return;
    }
    if (options.out) {
      writeFileSync(options.out, text);
      return;
    }
    process.stdout.write(text);
    return;
  }

  if (
    !options.version ||
    !options.dir ||
    !options.pubkey ||
    !options.out ||
    !options.baseUrl
  ) {
    throw new Error(
      "missing arguments; see the usage at the top of scripts/release/v2-manifest.mjs",
    );
  }
  const dir = resolve(options.dir);
  const publicKey = parsePublicKey(readFileSync(options.pubkey, "utf8"));
  const assets = options.assets
    ? JSON.parse(readFileSync(options.assets, "utf8"))
    : undefined;
  if (assets?.some((asset) => asset.name === "latest.json")) {
    throw new Error(
      "the release has a latest.json asset; 2.x must never publish one",
    );
  }

  const urlFor = (name) => {
    if (assets) {
      const asset = assets.find((candidate) => candidate.name === name);
      if (!asset) {
        throw new Error(`${name} is not uploaded to the release`);
      }
      if (asset.size !== statSync(join(dir, name)).size) {
        throw new Error(
          `${name} on the release is ${asset.size} bytes, the built file ${statSync(join(dir, name)).size}`,
        );
      }
    }
    return `${options.baseUrl}/${encodeURIComponent(name)}`;
  };
  const signatureFor = (name) => {
    const path = join(dir, name);
    if (!existsSync(path) || !existsSync(`${path}.sig`)) {
      throw new Error(`${name} or its .sig is missing`);
    }
    const signature = readFileSync(`${path}.sig`, "utf8").trim();
    const trusted = verifySignature(readFileSync(path), signature, publicKey);
    // kwikpaste-updater refuses a signature whose trusted comment names another file.
    if (!trusted.split("\t").includes(`file:${name}`)) {
      throw new Error(
        `${name}.sig was made for another file (trusted comment: ${trusted})`,
      );
    }
    return signature;
  };

  const manifest = buildManifest({
    notes: options.notes ? readFileSync(options.notes, "utf8").trim() : "",
    pubDate: options.pubDate ?? new Date().toISOString(),
    signatureFor,
    urlFor,
    version: options.version,
  });
  writeFileSync(options.out, serialize(manifest));
  process.stdout.write(
    `ok  ${options.out}: ${options.version}, ${Object.keys(manifest.platforms).length} platforms, signatures verify with ${publicKey.keyId}\n`,
  );
};

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  try {
    main();
  } catch (error) {
    process.stdout.write(`::error::${error.message}\n`);
    process.exit(1);
  }
}
