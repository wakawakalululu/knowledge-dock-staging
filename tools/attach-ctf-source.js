// 注入 Des-CTF-Knowledge git 源 + 知识根（仓库已预先克隆到 repos/des-ctf-knowledge）
// 写盘前按 BASELINE §7 调用 import_diag 校验完整 Config schema。
const fs = require("fs");
const os = require("os");
const path = require("path");
const { spawnSync } = require("child_process");

if (!process.env.APPDATA) {
  console.error("APPDATA is not set");
  process.exit(1);
}

const appDir = path.join(process.env.APPDATA, "com.local.knowledgedock");
const configPath = path.join(appDir, "config.json");
const reposDir = path.join(appDir, "repos");
const srcDir = path.join(reposDir, "des-ctf-knowledge");

if (!fs.existsSync(path.join(srcDir, ".git"))) {
  console.error("repo not cloned at " + srcDir);
  process.exit(1);
}

const currentJson = fs.readFileSync(configPath, "utf8");
const cfg = JSON.parse(currentJson);
cfg.git_sources = cfg.git_sources || [];
cfg.roots = cfg.roots || [];

if (!cfg.git_sources.some((s) => s.name === "des-ctf-knowledge")) {
  cfg.git_sources.push({
    name: "des-ctf-knowledge",
    url: "https://github.com/Dest1ny-Sec/Des-CTF-Knowledge.git",
    branch: "",
    enabled: true,
    interval_min: 0,
    last_sync: null,
    last_error: null,
  });
}
if (!cfg.roots.some((r) => r.label === "des-ctf-knowledge")) {
  cfg.roots.push({
    path: srcDir,
    label: "des-ctf-knowledge",
    enabled: true,
    root_exclude_globs: [".git/", "assets/", "mcp/"],
    root_exts: [],
    root_max_file_kb: 0,
  });
}

const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "kd-import-diag-"));
const incomingPath = path.join(tempDir, "incoming.json");
let injected = false;
try {
  fs.writeFileSync(incomingPath, JSON.stringify(cfg, null, 2));
  const check = spawnSync(
    "cargo",
    ["run", "--bin", "import_diag", "--", configPath, incomingPath],
    {
      cwd: path.resolve(__dirname, "../src-tauri"),
      encoding: "utf8",
    },
  );
  if (check.stdout) process.stdout.write(check.stdout);
  if (check.stderr) process.stderr.write(check.stderr);
  if (check.error || check.status !== 0) {
    console.error("import_diag failed; config.json was not modified");
  } else {
    const backupDir = path.join(appDir, "config-backups");
    fs.mkdirSync(backupDir, { recursive: true });
    const ts = new Date().toISOString().replace(/[:.]/g, "-");
    const backupPath = path.join(backupDir, `config-${ts}.json`);
    fs.copyFileSync(configPath, backupPath);
    const backups = fs.readdirSync(backupDir)
      .filter((name) => name.startsWith("config-") && name.endsWith(".json"))
      .sort();
    while (backups.length > 5) {
      fs.unlinkSync(path.join(backupDir, backups.shift()));
    }

    fs.writeFileSync(configPath, JSON.stringify(cfg, null, 2));
    injected = true;
    console.log(
      "ctf injected: git_sources=" + cfg.git_sources.length,
      "roots=" + cfg.roots.length,
      "backup=" + backupPath,
    );
  }
} finally {
  fs.rmSync(tempDir, { recursive: true, force: true });
}
if (!injected) process.exit(1);
