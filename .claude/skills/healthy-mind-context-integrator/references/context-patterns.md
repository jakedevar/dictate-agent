# Context Patterns Reference

## Complete AuthContext Example

From `ui/src/context/auth.rs`:

### State Struct
```rust
#[derive(Clone, PartialEq)]
pub struct AuthState {
    pub user: Option<User>,
    pub session_id: Option<String>,
    pub is_loading: bool,
    pub is_authenticated: bool,
    pub init_error: Option<String>,
}

impl Default for AuthState {
    fn default() -> Self {
        Self {
            user: None,
            session_id: None,
            is_loading: true,  // Start loading
            is_authenticated: false,
            init_error: None,
        }
    }
}
```

### Context Struct
```rust
#[derive(Clone, Copy)]
pub struct AuthContext {
    pub state: Signal<AuthState>,
}

impl AuthContext {
    pub fn new() -> Self {
        Self {
            state: Signal::new(AuthState::default()),
        }
    }
}
```

### Async Init Method
```rust
impl AuthContext {
    pub async fn init(&mut self) {
        self.state.write().is_loading = true;
        self.state.write().init_error = None;

        // Async operations here
        match some_async_operation().await {
            Ok(data) => {
                self.state.write().data = Some(data);
            }
            Err(e) => {
                self.state.write().init_error = Some(e.to_string());
            }
        }

        self.state.write().is_loading = false;
    }
}
```

### State Update Methods
```rust
impl AuthContext {
    pub fn update_data(&mut self, data: DataType) {
        self.state.write().data = Some(data);
    }

    pub fn clear(&mut self) {
        *self.state.write() = AuthState::default();
    }

    // Getter that clones (for use in components)
    pub fn get_data(&self) -> Option<DataType> {
        self.state.read().data.clone()
    }
}
```

### Custom Hook
```rust
pub fn use_auth() -> AuthContext {
    use_context::<AuthContext>()
}
```

## Simpler ThemeContext Example

From `ui/src/theme.rs`:

```rust
#[derive(Clone, Copy)]
pub struct ThemeContext {
    pub mode: Signal<ThemeMode>,
}

pub fn use_theme() -> ThemeContext {
    use_context::<ThemeContext>()
}

#[component]
pub fn ThemeProvider(children: Element) -> Element {
    let mode = use_signal(|| ThemeMode::Glass);
    use_context_provider(|| ThemeContext { mode });
    rsx! { {children} }
}
```

## App Component Integration

```rust
#[component]
pub fn App() -> Element {
    // Initialize contexts
    let mut auth_context = use_context_provider(AuthContext::new);

    // Async init in effect
    use_effect(move || {
        spawn(async move {
            auth_context.init().await;
        });
    });

    rsx! {
        ThemeProvider {
            // ThemeProvider handles its own context
            Router::<Route> {}
        }
    }
}
```

## Key Points

1. **State struct**: `#[derive(Clone, PartialEq)]` for diffing
2. **Context struct**: `#[derive(Clone, Copy)]` for cheap passing
3. **Signal wrapping**: Context contains `Signal<State>`
4. **Read vs Write**: Use `.read()` for access, `.write()` for mutation
5. **Clone before await**: Always clone signal values before async operations
