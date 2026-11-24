// R47 高级语法测试：/api/search 语法断言
const B = "http://127.0.0.1:18230/api/search";
let pass = 0, total = 0;
async function run(name, fn) {
  total++;
  try { await fn(); pass++; console.log("PASS - " + name); }
  catch (e) { console.log("FAIL - " + name + " : " + e.message); }
}
const assert = (c, m) => { if (!c) throw new Error(m); };
const get = async (q, limit = 10) => (await (await fetch(B + "?q=" + encodeURIComponent(q) + "&limit=" + limit)).json());
(async () => {
  await run("普通词基线（模板注入 → SSTI.md Top5）", async () => {
    const j = await get("模板注入");
    assert(j.ok && j.hits.length > 0, "无结果");
    assert(j.hits.slice(0, 5).some(h => h.path.includes("SSTI")), "Top5 无 SSTI");
  });
  await run("精确短语 \"模板注入\"", async () => {
    const j = await get('"模板注入"');
    assert(j.ok && j.hits.length > 0, "无结果");
    assert(j.hits.some(h => h.path.includes("SSTI")), "无 SSTI");
  });
  await run("短语精确性：\"zzdebounce42\"", async () => {
    const j = await get('"zzdebounce42"');
    assert(j.ok, "ok=false");
  });
  await run("-排除：模板注入 -wp（排除 WP 文章）", async () => {
    const j = await get("模板注入 -wp");
    assert(j.ok && j.hits.length > 0, "无结果");
    assert(j.hits.every(h => !h.path.toLowerCase().includes("wp")), "结果含 wp 文件");
  });
  await run("title:SSTI（仅标题）", async () => {
    const j = await get("title:SSTI");
    assert(j.ok && j.hits.length > 0, "无结果");
    assert(j.hits.every(h => h.title.toLowerCase().includes("ssti") || h.path.toLowerCase().includes("ssti")), "非 SSTI 标题");
  });
  await run("path:techniques（路径过滤）", async () => {
    const j = await get("注入 path:techniques");
    assert(j.ok, "ok=false");
    assert(j.hits.every(h => h.path.toLowerCase().includes("techniques")), "含非 techniques 路径");
  });
  await run("type:manual 语法过滤", async () => {
    const j = await get("注入 type:manual");
    assert(j.ok && j.hits.length > 0, "无结果");
    assert(j.hits.every(h => h.doc_type === "Manual"), "含非 Manual");
  });
  await run("root:des-ctf-knowledge 语法过滤", async () => {
    const j = await get("模板注入 root:des-ctf-knowledge");
    assert(j.ok && j.hits.length > 0, "无结果");
    assert(j.hits.every(h => h.root === "des-ctf-knowledge"), "含非 CTF 根");
  });
  await run("mtime:>1d（最近修改）", async () => {
    const j = await get("注入 mtime:>1d");
    assert(j.ok, "ok=false");
    assert(j.hits.length > 0, "最近 1 天应命中（库 3 小时前导入）");
  });
  await run("组合：\"模板注入\" root:des-ctf-knowledge type:manual -articles", async () => {
    const j = await get('"模板注入" root:des-ctf-knowledge type:manual -articles');
    assert(j.ok, "ok=false");
    assert(j.hits.every(h => h.root === "des-ctf-knowledge" && h.doc_type === "Manual" && !h.path.includes("articles")), "组合条件不满足");
    console.log("      (" + j.hits.length + " 条组合结果)");
  });
  console.log(`\n${pass}/${total} 通过`);
  if (pass !== total) process.exitCode = 1;
})();
