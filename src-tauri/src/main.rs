//! 知识悬浮搜索 —— uTools 风格的本地知识库常驻置顶检索窗
//!
//! 逻辑主体在 lib.rs（便于集成测试复用），此文件仅保留 Windows GUI 入口。

fn main() {
    knowledge_dock_lib::run()
}
