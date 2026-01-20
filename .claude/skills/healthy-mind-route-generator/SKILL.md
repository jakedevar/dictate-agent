---
name: healthy-mind-route-generator
description: "Generate new routes for the Healthy Mind Dioxus application across web, desktop, and mobile platforms. Use when adding a new page or view to the application. Handles route enum variants, layout wrapping for protected routes, route component functions, and shared UI component scaffolding. Triggers: add a route, create a new page, add a view for, new protected route."
---

# Healthy Mind Route Generator

Generate routes across all 3 platforms (web, desktop, mobile) following established patterns.

## Workflow

1. Determine route requirements:
   - Path (e.g., "/appointments", "/patients/:id/notes")
   - Protected or public (protected routes use layout wrapping)
   - Dynamic parameters (if any)
   - Shared UI component name

2. Update route files in order:
   - `web/src/lib.rs` - Web route enum and component
   - `desktop/src/lib.rs` - Desktop route enum and component
   - `mobile/src/lib.rs` - Mobile route enum and component
   - `ui/src/` - Shared view component (if creating new)

3. For protected routes, add within layout block:
   - Web: `#[layout(WebLayout)]` ... `#[end_layout]`
   - Desktop: `#[layout(DesktopLayout)]` with indentation
   - Mobile: `#[layout(MobileLayout)]` with indentation

## Route Patterns

See `references/route-patterns.md` for complete platform-specific patterns.

## Quick Reference

**Static Route:**
```rust
#[route("/path")]
VariantName,
```

**Dynamic Route:**
```rust
#[route("/path/:param")]
VariantName { param: String },
```

**Route Component:**
```rust
#[component]
fn VariantName() -> Element {
    rsx! { SharedComponent {} }
}
```

**With Parameters:**
```rust
#[component]
fn VariantName(param: String) -> Element {
    rsx! { SharedComponent { param } }
}
```

## Platform Differences

- **Web**: No `#[rustfmt::skip]`, routes not indented in layout
- **Desktop/Mobile**: Use `#[rustfmt::skip]`, indent routes within layout block
- **Variant naming**: Web uses varied names, Desktop/Mobile use `*Route` suffix consistently
