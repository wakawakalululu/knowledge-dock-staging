const fs = require("fs");
const path = require("path");

const [mode, ...args] = process.argv.slice(2);

function readJson(file) {
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

function walkFiles(dir) {
  if (!fs.existsSync(dir)) return [];
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...walkFiles(full));
    else out.push(full);
  }
  return out;
}

switch (mode) {
  case "find-run": {
    const data = readJson(args[0]);
    const sha = args[1];
    const successOnly = args[2] === "success";
    const run = (data.workflow_runs || []).find(
      (item) => item.head_sha === sha && (!successOnly || item.conclusion === "success"),
    );
    if (!run) process.exit(2);
    console.log(
      [run.id, run.status, run.conclusion || "-", run.html_url, run.head_sha].join("\t"),
    );
    break;
  }

  case "summary": {
    const run = readJson(args[0]);
    console.log([run.status, run.conclusion || "-", run.html_url, run.head_sha].join("\t"));
    break;
  }

  case "artifact": {
    const data = readJson(args[0]);
    const name = args[1];
    const artifact = (data.artifacts || []).find((item) => item.name === name);
    if (!artifact) process.exit(2);
    console.log([artifact.id, artifact.name, artifact.size_in_bytes].join("\t"));
    break;
  }

  case "failed-jobs": {
    const data = readJson(args[0]);
    for (const job of data.jobs || []) {
      if (job.conclusion === "success") continue;
      console.log(`JOB_FAILED ${job.name}\t${job.conclusion || job.status}`);
      for (const step of job.steps || []) {
        if (step.conclusion && step.conclusion !== "success") {
          console.log(`STEP_FAILED ${step.name}\t${step.conclusion}`);
        }
      }
    }
    break;
  }

  case "filter-log": {
    const dir = args[0];
    const limit = Number(args[1] || 120);
    const pattern = /panic|error|failed|failure|exception/i;
    const matches = [];
    for (const file of walkFiles(dir)) {
      const lines = fs.readFileSync(file, "utf8").split(/\r?\n/);
      lines.forEach((line, index) => {
        if (pattern.test(line)) {
          matches.push(`${path.relative(dir, file)}:${index + 1}: ${line.slice(0, 2000)}`);
        }
      });
    }
    for (const line of matches.slice(0, limit)) console.log(line);
    console.log(`LOG_MATCHES ${matches.length} (printed ${Math.min(limit, matches.length)})`);
    break;
  }

  case "filter-file": {
    const file = args[0];
    const limit = Number(args[1] || 40);
    const pattern = /panic|error|failed|failure|exception/i;
    if (!fs.existsSync(file)) {
      console.log("(missing log)");
      break;
    }
    const matches = fs.readFileSync(file, "utf8")
      .split(/\r?\n/)
      .map((line, index) => ({ line, index }))
      .filter(({ line }) => pattern.test(line))
      .map(({ line, index }) => `${index + 1}: ${line.slice(0, 2000)}`);
    for (const line of matches.slice(0, limit)) console.log(line);
    console.log(`LOG_MATCHES ${matches.length} (printed ${Math.min(limit, matches.length)})`);
    break;
  }

  case "tail-log": {
    const file = args[0];
    const limit = Number(args[1] || 40);
    if (!fs.existsSync(file) || fs.statSync(file).size === 0) {
      console.log("(empty log)");
      break;
    }
    const lines = fs.readFileSync(file, "utf8").split(/\r?\n/);
    if (lines.at(-1) === "") lines.pop();
    for (const line of lines.slice(-limit)) console.log(line.slice(0, 2000));
    break;
  }

  default:
    console.error(`unknown mode: ${mode}`);
    process.exit(2);
}
