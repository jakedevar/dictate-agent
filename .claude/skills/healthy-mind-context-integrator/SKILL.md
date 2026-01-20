---
name: healthy-mind-context-integrator
description: "Add new global context providers to the Healthy Mind Dioxus application following the AuthContext pattern. Use when creating app-wide state like notifications, caching, or feature flags. Handles state struct, context struct, custom hooks, provider initialization, and async init patterns. Triggers: add context, create global state, new context provider, app-wide state."
---

# Healthy Mind Context Integrator

Add global contexts following the AuthContext pattern.

## Workflow

1. Determine context requirements:
   - State fields needed
   - Async initialization required?
   - Methods for state updates

2. Create context file:
   - State struct with Default
   - Context struct with Signal
   - Custom hook function
   - Methods for state access/update

3. Integrate in App components:
   - Add `use_context_provider` call
   - Add `use_effect` for async init (if needed)

## Context Pattern

See `references/context-patterns.md` for complete patterns.

## Quick Reference

**State Struct:**
```rust
#[derive(Clone, PartialEq)]
pub struct MyState {
    pub data: Option<DataType>,
    pub is_loading: bool,
    pub error: Option<String>,
}

impl Default for MyState {
    fn default() -> Self {
        Self {
            data: None,
            is_loading: false,
            error: None,
        }
    }
}
```

**Context Struct:**
```rust
#[derive(Clone, Copy)]
pub struct MyContext {
    pub state: Signal<MyState>,
}

impl MyContext {
    pub fn new() -> Self {
        Self {
            state: Signal::new(MyState::default()),
        }
    }
}
```

**Custom Hook:**
```rust
pub fn use_my_context() -> MyContext {
    use_context::<MyContext>()
}
```

**App Integration:**
```rust
#[component]
pub fn App() -> Element {
    let mut my_context = use_context_provider(MyContext::new);

    use_effect(move || {
        spawn(async move {
            my_context.init().await;
        });
    });

    rsx! { /* ... */ }
}
```

## Files to Modify

1. Create: `ui/src/context/{name}.rs`
2. Export in: `ui/src/context/mod.rs` (create if needed)
3. Export in: `ui/src/lib.rs`
4. Initialize in: `web/src/lib.rs`, `desktop/src/lib.rs`, `mobile/src/lib.rs`
