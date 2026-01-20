# Route Patterns Reference

## Web Platform (web/src/lib.rs)

### Route Enum Structure
```rust
#[derive(Debug, Clone, Routable, PartialEq)]
pub enum Route {
    // Public routes (no layout)
    #[route("/")]
    Landing,

    #[route("/login")]
    Login,

    // Protected routes (with layout)
    #[layout(WebLayout)]
    #[route("/dashboard")]
    Home,

    #[route("/patients/:id")]
    ProviderPatientDetailRoute { id: String },
    #[end_layout]

    // Catch-all (must be last)
    #[route("/:..segments")]
    NotFound { segments: Vec<String> },
}
```

### Route Component Pattern
```rust
#[component]
fn VariantName() -> Element {
    rsx! { SharedUIComponent {} }
}

#[component]
fn VariantWithParam(id: String) -> Element {
    rsx! { SharedUIComponent { patient_id: id } }
}
```

## Desktop/Mobile Platform

### Route Enum Structure (with rustfmt skip)
```rust
#[derive(Debug, Clone, Routable, PartialEq)]
#[rustfmt::skip]
pub enum Route {
    // Public routes
    #[route("/")]
    Landing,

    // Protected routes (indented)
    #[layout(DesktopLayout)]
        #[route("/dashboard")]
        HomeRoute,

        #[route("/patients/:id")]
        PatientDetailRoute { id: String },
    #[end_layout]

    #[route("/:..segments")]
    NotFound { segments: Vec<String> },
}
```

## Layout Components

All layouts must include `Outlet::<Route> {}` for child rendering:

```rust
#[component]
fn WebLayout() -> Element {
    rsx! {
        NavbarLayout {
            sidebar: rsx! { Sidebar { /* props */ } },
            header: rsx! { AppHeader { /* props */ } },
            Outlet::<Route> {}
        }
    }
}
```

## Adding Navigation Items

Update layout component's `nav_items` vector:
```rust
let nav_items = vec![
    NavItem { label: "Dashboard", href: "/dashboard", icon: NavIcon::Dashboard },
    NavItem { label: "New Item", href: "/new-path", icon: NavIcon::Settings },
];
```

## Current Path Matching

Update route match in layout for active state:
```rust
let current_path = match route {
    Route::HomeRoute => "/dashboard",
    Route::NewRoute => "/new-path",
    _ => "",
};
```
