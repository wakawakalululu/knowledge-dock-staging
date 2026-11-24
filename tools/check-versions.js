const fs = require("fs");
const path = require("path");

const root = path.resolve(__dirname, "..");
const read = (relative) => fs.readFileSync(path.join(root, relative), "utf8");
const pkg = JSON.parse(read("package.json"));
const lock = JSON.parse(read("package-lock.json"));
const tauri = JSON.parse(read("src-tauri/tauri.conf.json"));
const cargo = read("src-tauri/Cargo.toml");
const cargoLock = read("src-tauri/Cargo.lock");

const cargoVersion = cargo.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
const packageBlocks = cargoLock.split("[[package]]").slice(1);
const dockBlock = packageBlocks.find((block) => /^name\s*=\s*"knowledge-dock"$/m.test(block));
const cargoLockVersion = dockBlock?.match(/^version\s*=\s*"([^"]+)"/m)?.[1];

const checks = [
  ["package.json", pkg.version],
  ["package-lock.json", lock.version],
  ["package-lock.json packages[\"\"]", lock.packages?.[""]?.version],
  ["src-tauri/tauri.conf.json", tauri.version],
  ["src-tauri/Cargo.toml", cargoVersion],
  ["src-tauri/Cargo.lock", cargoLockVersion],
];

const expected = pkg.version;
const failures = checks.filter(([, value]) => value !== expected);
if (failures.length) {
  for (const [name, value] of failures) {
    console.error(`VERSION_MISMATCH ${name}: ${value || "missing"} != ${expected}`);
  }
  process.exit(1);
}

console.log(`VERSION_OK ${expected} (${checks.length} locations)`);
