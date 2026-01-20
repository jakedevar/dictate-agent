---
name: healthy-mind-debug-assistant
description: "Guide debugging of common issues in the Healthy Mind Dioxus application. Covers session persistence problems, authentication state issues, component rendering problems, and server function errors. Provides structured investigation workflows and relevant file locations. Triggers: debug, why is not, not working, investigate, troubleshoot."
---

# Healthy Mind Debug Assistant

Structured debugging workflows for common issues.

## Issue Categories

### 1. Session/Authentication Issues

**Symptoms:**
- User logged out unexpectedly
- Session not persisting across refresh
- "Not authenticated" errors

**Investigation Steps:**

1. Check browser storage:
   ```javascript
   // In browser console
   localStorage.getItem('healthy_mind_session')
   ```

2. Check auth context state:
   - `ui/src/context/auth.rs:43-85` - `init()` method
   - Look for `is_permanent_auth_failure` vs transient errors

3. Check server session validation:
   - `api/src/auth.rs` - `validate_session()` method
   - Check session expiration logic

4. Check database:
   ```sql
   SELECT * FROM sessions WHERE id = 'session_id';
   SELECT * FROM sessions WHERE user_id = 'user_id' ORDER BY created_at DESC;
   ```

**Common Fixes:**
- Clear localStorage and re-login
- Check session expiration time (default 7 days)
- Verify database connection

### 2. Component Not Rendering

**Symptoms:**
- Blank page or section
- Component shows but data is missing
- Infinite loading state

**Investigation Steps:**

1. Check browser console for errors

2. Check route configuration:
   - `{platform}/src/lib.rs` - Route enum
   - Verify route component function exists

3. Check component props:
   - Are required props passed?
   - Are signals initialized?

4. Check server function calls:
   - Add `console::log!()` or `log_debug()` calls
   - Check network tab for request/response

**Common Fixes:**
- Ensure component is exported in `ui/src/lib.rs`
- Check import paths
- Verify signal initialization

### 3. Server Function Errors

**Symptoms:**
- "ServerFnError" in console
- API calls failing
- Data not saving

**Investigation Steps:**

1. Check server logs (terminal running `dx serve`)

2. Verify function signature:
   - `api/src/lib.rs` - Server function definitions
   - Check parameter types match

3. Check authentication:
   - Is session_id being passed?
   - Is `validate_session()` being called?

4. Check database operations:
   - `api/src/storage.rs` - Data access methods
   - Run query manually to verify

**Common Fixes:**
- Clone values before await
- Add proper error handling
- Check database constraints

### 4. Styling Issues

**Symptoms:**
- Styles not applying
- Different appearance per platform
- Tailwind classes not working

**Investigation Steps:**

1. Check CSS is registered:
   - Asset constant in lib.rs
   - document::Link in App component

2. Check class names:
   - Tailwind only works on web (CDN)
   - Custom CSS needs to be in assets/

3. Check CSS specificity:
   - Inspect element in browser
   - Look for overriding styles

**Common Fixes:**
- Register CSS in all platforms
- Use custom CSS for cross-platform consistency
- Check for typos in class names

## Useful Debug Commands

```bash
# Check for compilation errors
cargo check --workspace

# Run with verbose logging
RUST_LOG=debug dx serve

# Check database state
psql $DATABASE_URL -c "SELECT * FROM users LIMIT 5;"

# Clear and rebuild
cargo clean && dx serve
```

## Key Files for Debugging

| Issue Type | Files to Check |
|------------|----------------|
| Auth | `ui/src/context/auth.rs`, `api/src/auth.rs` |
| Routes | `{platform}/src/lib.rs` |
| Server Functions | `api/src/lib.rs`, `api/src/storage.rs` |
| Components | `ui/src/components/` |
| Database | `api/src/db.rs`, `api/migrations/` |
