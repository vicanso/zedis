# Macos未公开函数替换

由于`gpui/src/platform/macos/window.rs`使用了macos的以下未公开函数，因此需要替换为空函数。

```rust
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    // Widely used private APIs; Apple uses them for their Terminal.app.
    fn CGSMainConnectionID() -> id;
    fn CGSSetWindowBackgroundBlurRadius(
        connection_id: id,
        window_id: NSInteger,
        radius: i64,
    ) -> i32;
}

```

替换代码如下：


```rust
// =========================================================
// 替换开始：使用空函数 (Stub) 替代私有 API
// =========================================================

// 定义一个假的 CGSMainConnectionID
// #[allow(non_snake_case)] 告诉编译器不要报命名风格警告（因为我们要模仿 C 函数名）
#[allow(non_snake_case)]
unsafe fn CGSMainConnectionID() -> id {
    // 返回空指针，假装获取失败或无操作
    std::ptr::null_mut()
}

// 定义一个假的 CGSSetWindowBackgroundBlurRadius
#[allow(non_snake_case)]
unsafe fn CGSSetWindowBackgroundBlurRadius(
    _connection_id: id,
    _window_id: NSInteger,
    _radius: i64,
) -> i32 {
    // 这里什么都不做。
    // _ 前缀告诉编译器忽略未使用的变量。

    // 返回 0 (通常代表 kCGErrorSuccess)，假装调用成功了，
    // 这样调用方的错误处理逻辑不会崩溃。
    0
}

// =========================================================
// 替换结束
// =========================================================

```