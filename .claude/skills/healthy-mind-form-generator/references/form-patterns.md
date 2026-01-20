# Form Patterns Reference

## Signal Initialization

### Text Fields
```rust
let mut username = use_signal(String::new);
let mut email = use_signal(String::new);
```

### Boolean Fields
```rust
let mut remember_me = use_signal(|| false);
let mut show_dropdown = use_signal(|| false);
```

### Optional Enum Fields
```rust
let mut gender = use_signal(|| None::<GenderType>);
let mut status = use_signal(|| None::<StatusType>);
```

### State Management
```rust
let mut error_message = use_signal(|| None::<String>);
let mut is_loading = use_signal(|| false);
let mut selected_index = use_signal(|| 0);
```

## Complete Submit Handler Example

From `ui/src/components/patient/new_patient_modal.rs`:

```rust
let handle_submit = move |evt: Event<FormData>| {
    evt.prevent_default();
    spawn(async move {
        is_loading.set(true);
        error_message.set(None);

        // Validate authentication first
        let session_id = match auth.session_id() {
            Some(id) => id,
            None => {
                error_message.set(Some("Not authenticated".to_string()));
                is_loading.set(false);
                return;
            }
        };

        // Clone all values before validation/await
        let first_name_val = first_name.read().clone();
        let last_name_val = last_name.read().clone();
        let email_val = email.read().clone();

        // Validate with early returns (no nesting!)
        if first_name_val.trim().is_empty() {
            error_message.set(Some("First name is required".to_string()));
            is_loading.set(false);
            return;
        }
        if last_name_val.trim().is_empty() {
            error_message.set(Some("Last name is required".to_string()));
            is_loading.set(false);
            return;
        }

        // Call server function
        match api::create_patient(session_id, first_name_val, last_name_val, email_val).await {
            Ok(patient_id) => {
                on_created.call(patient_id);
            }
            Err(e) => {
                error_message.set(Some(format!("{}", e)));
                is_loading.set(false);
            }
        }
    });
};
```

## Input Component Usage

### Using Input Component (from ui/src/components/)
```rust
Input {
    label: "Username".to_string(),
    value: username.read().clone(),
    onchange: move |val| username.set(val),
    placeholder: "Enter username".to_string(),
    required: true,
    autofocus: true,
}
```

### Using Raw input Element
```rust
input {
    class: "w-full px-4 py-3 bg-[#0f172a] border border-slate-700 rounded-lg",
    r#type: "text",
    placeholder: "First name",
    value: "{first_name}",
    oninput: move |evt| first_name.set(evt.value()),
    required: true,
}
```

### Checkbox Input
```rust
input {
    r#type: "checkbox",
    id: "remember",
    checked: *remember_me.read(),
    onchange: move |evt| remember_me.set(evt.checked()),
}
```

## Error Display Pattern

```rust
if let Some(ref err) = *error_message.read() {
    div {
        class: "mb-6 p-4 bg-red-500/10 border border-red-500/20 rounded-lg text-red-400",
        "{err}"
    }
}
```

## Loading State Pattern

### Button with Loading
```rust
Button {
    variant: ButtonVariant::Primary,
    loading: *is_loading.read(),
    "Submit"
}
```

### Custom Loading Spinner
```rust
button {
    r#type: "submit",
    disabled: *is_loading.read(),
    if *is_loading.read() {
        div { class: "w-4 h-4 border-2 border-white/30 border-t-white rounded-full animate-spin" }
    }
    "Submit"
}
```

## Form Element Structure

```rust
form {
    class: "space-y-6",
    onsubmit: handle_submit,

    // Error banner
    if let Some(ref err) = *error_message.read() {
        div { class: "error-banner", "{err}" }
    }

    // Form fields
    div { class: "form-group",
        label { "Field Label" }
        input { /* ... */ }
    }

    // Submit button
    button {
        r#type: "submit",
        disabled: *is_loading.read(),
        "Submit"
    }
}
```
