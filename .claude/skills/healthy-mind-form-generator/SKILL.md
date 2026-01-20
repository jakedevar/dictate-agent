---
name: healthy-mind-form-generator
description: "Generate form components for the Healthy Mind Dioxus application with proper signal-based state management, validation, async submission, and server function integration. Use when creating forms for user input such as patient data, settings, or any data entry. Handles signal initialization, validation with early returns, spawn() async patterns, loading/error states, and server function calls. Triggers: create a form, add a form for, generate input form, scaffold a form."
---

# Healthy Mind Form Generator

Generate forms following established Dioxus patterns with signals, validation, and async submission.

## Workflow

1. Determine form requirements:
   - Fields and their types (text, email, password, checkbox, select)
   - Required vs optional fields
   - Server function to call on submit
   - Success behavior (navigate, close modal, callback)

2. Generate component with:
   - Signal initialization for each field
   - Error and loading state signals
   - Validation logic with early returns
   - Async submit handler with spawn()
   - Server function call with error handling

## Form Patterns

See `references/form-patterns.md` for complete patterns.

## Quick Reference

**Signal Initialization:**
```rust
let mut field_name = use_signal(String::new);
let mut checkbox = use_signal(|| false);
let mut optional_enum = use_signal(|| None::<EnumType>);
let mut error_message = use_signal(|| None::<String>);
let mut is_loading = use_signal(|| false);
```

**Submit Handler Structure:**
```rust
let handle_submit = move |evt: Event<FormData>| {
    evt.prevent_default();
    spawn(async move {
        is_loading.set(true);
        error_message.set(None);

        // Clone values before await
        let field_val = field_name.read().clone();

        // Validate with early returns
        if field_val.trim().is_empty() {
            error_message.set(Some("Field is required".to_string()));
            is_loading.set(false);
            return;
        }

        // Call server function
        match server_function(field_val).await {
            Ok(result) => { /* success handling */ }
            Err(e) => {
                error_message.set(Some(format!("{}", e)));
                is_loading.set(false);
            }
        }
    });
};
```

**Input Binding:**
```rust
input {
    value: "{field_name}",
    oninput: move |evt| field_name.set(evt.value()),
    required: true,
}
```

## Important Rules

1. **Always clone signal values before await** - prevents holding read guards
2. **Use early returns for validation** - never nest if statements
3. **Reset loading only on error** - success typically navigates away
4. **Validate authentication first** for protected operations
