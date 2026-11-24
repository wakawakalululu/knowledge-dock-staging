// 答题式检索测试 v2：模拟外部答题程序调用 knowledge-dock HTTP API
// 用例：主题命中断言 + root 过滤 + ai=1 筛选（AI 启用时）
const base = "http://127.0.0.1:18230/api/search";
let pass = 0;
let total = 0;
async function run(name, fn) {
  total++;
  try {
    await fn();
    pass++;
    console.log("PASS - " + name);
  } catch (e) {
    console.log("FAIL - " + name + " : " + e.message);
  }
}
function assert(cond, msg) {
  if (!cond) throw new Error(msg);
}
(async () => {
  // 1-5. 主题命中
  const cases = [
    { q: "SSTI 模板注入 检测 payload", expect: "SSTI" },
    { q: "{{7*7}} 49 模板", expect: "SSTI" },
    { q: "JWT 密钥 爆破 工具", expect: "JWT" },
    { q: "图片隐写 工具 用法", expect: "图片隐写" },
    { q: "文件上传 绕过 后缀", expect: "上传" },
  ];
  for (const c of cases) {
    await run(`主题命中 "${c.q}" → ${c.expect}`, async () => {
      const r = await fetch(base + "?q=" + encodeURIComponent(c.q) + "&limit=5");
      const j = await r.json();
      assert(j.ok, "ok=false");
      assert(j.hits.some((h) => h.path.toLowerCase().includes(c.expect.toLowerCase())), `Top5 无 ${c.expect}`);
      assert(!j.hits.some((h) => h.path.includes(".idx")), "结果含 idx 文件");
    });
  }
  // 6. root 过滤：限定 biglib
  await run("root=biglib 过滤", async () => {
    const r = await fetch(base + "?q=" + encodeURIComponent("大库测试 概述") + "&limit=5&root=biglib");
    const j = await r.json();
    assert(j.ok && j.hits.length > 0, "无结果");
    assert(j.hits.every((h) => h.root === "biglib"), "含非 biglib 结果");
  });
  // 7. type 过滤
  await run("type=Manual 过滤", async () => {
    const r = await fetch(base + "?q=" + encodeURIComponent("SSTI") + "&limit=5&type=Manual");
    const j = await r.json();
    assert(j.ok, "ok=false");
    assert(j.hits.every((h) => h.doc_type === "Manual"), "含非 Manual 结果");
  });
  // 8. ai=1（AI 启用时）：ai_used=true 且 keep 有效
  await run("ai=1 AI 筛选", async () => {
    const r = await fetch(base + "?q=" + encodeURIComponent("SSTI 模板注入") + "&limit=10&ai=1");
    const j = await r.json();
    assert(j.ok, "ok=false");
    assert(typeof j.ai_used === "boolean", "缺 ai_used 字段");
    if (j.ai_used) {
      assert(j.hits.length > 0 && j.hits.length <= 10, "ai 筛选后数量异常");
      console.log("      (AI 已启用: " + j.hits.length + " 条精选)");
    } else {
      console.log("      (AI 未启用，静默回退全量)");
    }
  });
  // 9. 无 token 时未启用鉴权直通（默认配置）
  await run("health 探针", async () => {
    const r = await fetch("http://127.0.0.1:18230/api/health");
    const j = await r.json();
    assert(j.ok && j.service === "knowledge-dock-api", "health 异常");
  });
  console.log(`\n${pass}/${total} 通过`);
  if (pass !== total) process.exitCode = 1;
})();
