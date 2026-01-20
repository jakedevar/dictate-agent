---
name: healthy-mind-css-asset-manager
description: "Manage CSS asset registration across web, desktop, and mobile platform crates in the Healthy Mind project. Use when adding new CSS files, registering existing CSS in additional platforms, or troubleshooting missing styles. Handles asset constant declaration, document::Link additions, and cross-platform consistency. Triggers: add CSS, register stylesheet, CSS not loading, add styles for."
---

# Healthy Mind CSS Asset Manager

Register CSS assets across all platform crates.

## Workflow

1. Determine CSS requirements:
   - New file or existing file?
   - Which platforms need it? (web, desktop, mobile, all)
   - File location within assets/

2. For each platform:
   - Add asset constant in lib.rs
   - Add document::Link in App component

## Asset Registration Pattern

### Step 1: Add Asset Constant

Location: `{platform}/src/lib.rs` (near top, with other asset constants)

```rust
pub const NEW_CSS: Asset = asset!("/assets/new-feature.css");
```

### Step 2: Add Document Link

Location: `{platform}/src/lib.rs` in `App` component's `rsx!` block

```rust
rsx! {
    ThemeProvider {
        // ... existing links ...
        document::Link { rel: "stylesheet", href: NEW_CSS }

        Router::<Route> {}
    }
}
```

## Platform-Specific Notes

### Web (`web/src/lib.rs`)
- Assets at lines 72-79
- Document links at lines 95-103
- May include external resources (Tailwind CDN, Google Fonts)

### Desktop (`desktop/src/lib.rs`)
- Assets at lines 62-68
- Document links at lines 207-213
- No external resources (offline-capable)

### Mobile (`mobile/src/lib.rs`)
- Assets at lines 60-67
- Document links at lines 199-206
- Includes mobile-specific CSS (MOBILE_CSS)

## Common Issues

**CSS not loading:**
1. Check asset constant exists
2. Check document::Link is in App component
3. Check file exists in assets/ directory
4. Rebuild with `dx serve`

**Styles different per platform:**
- Web has Tailwind CDN (may override)
- Desktop/Mobile only have local styles
