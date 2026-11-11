// Packages an already built native exe with the pinned Tauri CLI (`tauri bundle`), the way 1.x was
// packaged, so 2.x installs over 1.x with the same identity.
//
//   Windows  NSIS setup (+ .sig) and the portable zip (+ .sig).
//   macOS    .app, .dmg and .app.tar.gz (+ .sig), renamed to the 1.x asset names.
//
// The exe must already exist at target/<triple>/release/KwikPaste[.exe] (built with
// `--features production-identity` for a release). Files land in --out with their asset names, plus the
// rendered NSIS script under nsis/<arch>/ for scripts/ci/check-nsis-render.mjs.
//
// --identity test  bundles under a throwaway identity (packaging/test-identity): another identifier,
//                  product name, binary name, publisher and file extension. Use it for anything that is
//                  installed on a developer machine: the real identity would replace the installed 1.x.
// --os-gate-test   (test identity only) also builds a setup whose OS gate requires build 99999.
// --pubkey <file>  the public key the CLI compares the signing key with, instead of
//                  crates/kwikpaste-updater/pubkey.txt (a throwaway key pair in dry runs).
// --features <list> pass Cargo features through to `tauri bundle` (for example,
//                    `e2e-overrides` in the local self-update driver).
// --no-sign        skips the updater signatures (no TAURI_SIGNING_PRIVATE_KEY).
//
// Usage: node scripts/release/package-native.mjs --target <triple> --out <dir> [options]
import { spawnSync } from "node:child_process";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
} from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

export const ROOT = resolve(import.meta.dirname, "..", "..");
export const APP_DIR = join(ROOT, "crates", "kwikpaste-app");
export const TAURI_CLI_VERSION = "2.11.4";

const say = (line) => {
  process.stdout.write(`${line}\n`);
};

/**
 * The version in `[workspace.package]` of the root Cargo.toml, which is what the CLI reads too.
 */
export const workspaceVersion = () => {
  const manifest = readFileSync(join(ROOT, "Cargo.toml"), "utf8");
  const section = manifest.split(/^\[workspace\.package\]\s*$/m)[1] ?? "";
  const match = section.match(/^version\s*=\s*"([^"]+)"/m);
  if (!match) {
    throw new Error("no version in [workspace.package] of Cargo.toml");
  }

  return match[1];
};

/**
 * Asset architecture names, as 1.x used them.
 */
export const targetInfo = (target) => {
  const table = {
    "aarch64-apple-darwin": {
      arch: "aarch64",
      os: "macos",
      platform: "aarch64",
    },
    "aarch64-pc-windows-msvc": {
      arch: "arm64",
      os: "windows",
      platform: "aarch64",
    },
    "x86_64-apple-darwin": { arch: "x64", os: "macos", platform: "x86_64" },
    "x86_64-pc-windows-msvc": {
      arch: "x64",
      os: "windows",
      platform: "x86_64",
    },
  };
  const info = table[target];
  if (!info) {
    throw new Error(`unsupported target ${target}`);
  }

  return info;
};

const run = (command, args, options = {}) => {
  say(`> ${command} ${args.join(" ")}`);
  const result = spawnSync(command, args, { stdio: "inherit", ...options });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    throw new Error(`${command} exited with ${result.status}`);
  }
};

/**
 * Path of the Tauri CLI entry script, after checking it is exactly the pinned version.
 */
export const tauriCli = () => {
  const script =
    process.env.KWIKPASTE_TAURI_CLI ??
    join(ROOT, "node_modules", "@tauri-apps", "cli", "tauri.js");
  const manifest = JSON.parse(
    readFileSync(join(script, "..", "package.json"), "utf8"),
  );
  if (manifest.version !== TAURI_CLI_VERSION) {
    throw new Error(
      `Tauri CLI is ${manifest.version}, the packaging is pinned to ${TAURI_CLI_VERSION}`,
    );
  }

  return script;
};

const parseArgs = (argv) => {
  const options = {
    features: undefined,
    identity: "production",
    osGateTest: false,
    pubkey: undefined,
    sign: true,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--target") {
      options.target = argv[++index];
    } else if (arg === "--out") {
      options.out = resolve(argv[++index]);
    } else if (arg === "--identity") {
      options.identity = argv[++index];
    } else if (arg === "--os-gate-test") {
      options.osGateTest = true;
    } else if (arg === "--pubkey") {
      options.pubkey = resolve(argv[++index]);
    } else if (arg === "--features") {
      options.features = argv[++index];
    } else if (arg === "--no-sign") {
      options.sign = false;
    } else {
      throw new Error(`unknown argument ${arg}`);
    }
  }
  if (!options.target || !options.out) {
    throw new Error(
      "Usage: package-native.mjs --target <triple> --out <dir> [--identity production|test] [--features <list>]",
    );
  }
  if (!["production", "test"].includes(options.identity)) {
    throw new Error(`unknown identity ${options.identity}`);
  }
  if (options.osGateTest && options.identity !== "test") {
    throw new Error("--os-gate-test only goes with --identity test");
  }

  return options;
};

const bundle = (cli, options, bundles, extraConfigs) => {
  const configs = [];
  if (options.identity === "test") {
    configs.push(join(ROOT, "packaging", "test-identity", "tauri.conf.json"));
  }
  if (options.pubkey) {
    configs.push(
      JSON.stringify({ plugins: { updater: { pubkey: options.pubkey } } }),
    );
  }
  configs.push(...extraConfigs);

  const args = [
    cli,
    "bundle",
    "--bundles",
    bundles,
    "--target",
    options.target,
    "--verbose",
  ];
  for (const config of configs) {
    args.push("--config", config);
  }
  if (!options.sign) {
    args.push("--no-sign");
  }
  if (options.features) {
    args.push("--features", options.features);
  }
  // The CLI looks for tauri.conf.json in TAURI_APP_PATH; from the repo root it would find src-tauri.
  run(process.execPath, args, {
    env: { ...process.env, TAURI_APP_PATH: APP_DIR },
  });
};

const copyWithSig = (from, to, sign) => {
  copyFileSync(from, to);
  if (sign) {
    copyFileSync(`${from}.sig`, `${to}.sig`);
  }
};

const packageWindows = (cli, options, version, info, release) => {
  const product =
    options.identity === "test" ? "KwikPasteBundleTest" : "KwikPaste";
  const exe = join(release, "KwikPaste.exe");
  if (!existsSync(exe)) {
    throw new Error(`${exe} does not exist; build it first`);
  }
  if (product !== "KwikPaste") {
    copyFileSync(exe, join(release, `${product}.exe`));
  }

  const produced = [];
  const setupName = `${product}_${version}_${info.arch}-setup.exe`;
  const builtSetup = join(release, "bundle", "nsis", setupName);
  const renderDir = join(release, "nsis", info.arch);

  bundle(cli, options, "nsis", []);
  copyWithSig(builtSetup, join(options.out, setupName), options.sign);
  produced.push(setupName);
  const renderOut = join(options.out, "nsis", info.arch);
  mkdirSync(renderOut, { recursive: true });
  for (const file of ["installer.nsi", "utils.nsh"]) {
    copyFileSync(join(renderDir, file), join(renderOut, file));
  }

  if (options.osGateTest) {
    const hooks = join(ROOT, "packaging", "test-identity", "os-gate-hooks.nsh");
    bundle(cli, { ...options, sign: false }, "nsis", [
      JSON.stringify({
        bundle: { windows: { nsis: { installerHooks: hooks } } },
      }),
    ]);
    // Kept out of the asset directory: it is a test fixture, not a release asset.
    const gateName = join(
      "tests",
      `${product}_${version}_${info.arch}-os-gate-test-setup.exe`,
    );
    mkdirSync(join(options.out, "tests"), { recursive: true });
    copyFileSync(builtSetup, join(options.out, gateName));
    produced.push(gateName);
  }

  // The portable zip always carries KwikPaste/KwikPaste.exe: the in-app updater and users rely on it.
  const shell = process.env.KWIKPASTE_POWERSHELL ?? "powershell";
  run(shell, [
    "-NoProfile",
    "-ExecutionPolicy",
    "Bypass",
    "-File",
    join(ROOT, "scripts", "package-portable.ps1"),
    "-ExePath",
    exe,
    "-Version",
    version,
    "-Arch",
    info.arch,
    "-OutDir",
    options.out,
  ]);
  const zipName = `KwikPaste_${version}_${info.arch}_portable.zip`;
  if (options.sign) {
    run(process.execPath, [cli, "signer", "sign", join(options.out, zipName)]);
  }
  produced.push(zipName);

  return produced;
};

const packageMacos = (cli, options, version, info, release) => {
  const product =
    options.identity === "test" ? "KwikPasteBundleTest" : "KwikPaste";
  const binary = join(release, "KwikPaste");
  if (!existsSync(binary)) {
    throw new Error(`${binary} does not exist; build it first`);
  }
  if (product !== "KwikPaste") {
    copyFileSync(binary, join(release, product));
  }

  bundle(cli, options, "app,dmg", []);

  const produced = [];
  const tarName = `${product}_${version}_${info.arch}.app.tar.gz`;
  // Signed again under the versioned name, like the portable zip: the updater checks the file name in
  // the trusted comment, and the bundler signed the unversioned `<product>.app.tar.gz`.
  const tar = join(options.out, tarName);
  copyFileSync(join(release, "bundle", "macos", `${product}.app.tar.gz`), tar);
  if (options.sign) {
    run(process.execPath, [cli, "signer", "sign", tar]);
  }
  produced.push(tarName);
  const dmgName = `${product}_${version}_${info.arch}.dmg`;
  copyFileSync(
    join(release, "bundle", "dmg", dmgName),
    join(options.out, dmgName),
  );
  produced.push(dmgName);

  return produced;
};

/**
 * `tauri bundle` accepts a key file path in TAURI_SIGNING_PRIVATE_KEY, `tauri signer sign` only the key
 * itself; pass the contents to both.
 */
const inlineSigningKey = () => {
  const key = process.env.TAURI_SIGNING_PRIVATE_KEY;
  if (key && existsSync(key)) {
    process.env.TAURI_SIGNING_PRIVATE_KEY = readFileSync(key, "utf8").trim();
  }
};

const main = () => {
  const options = parseArgs(process.argv.slice(2));
  inlineSigningKey();
  const version = workspaceVersion();
  const info = targetInfo(options.target);
  const cli = tauriCli();
  const release = join(ROOT, "target", options.target, "release");

  // Old bundles of another identity or version would otherwise be picked up below.
  rmSync(join(release, "bundle"), { force: true, recursive: true });
  mkdirSync(options.out, { recursive: true });

  const produced =
    info.os === "windows"
      ? packageWindows(cli, options, version, info, release)
      : packageMacos(cli, options, version, info, release);
  for (const name of produced) {
    say(`packaged ${join(options.out, name)}`);
  }
};

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  try {
    main();
  } catch (error) {
    say(`::error::${error.message}`);
    process.exit(1);
  }
}
