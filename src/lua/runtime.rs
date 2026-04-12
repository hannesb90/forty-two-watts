use mlua::{Lua, Result as LuaResult};

/// Memory limit for the Lua VM (4 MB — no ESP32 constraint).
const LUA_MEMORY_LIMIT: usize = 4 * 1024 * 1024;

/// Wrapper around the Lua VM with sandboxing and memory limits.
pub struct LuaRuntime {
    lua: Lua,
    memory_limit: usize,
}

impl LuaRuntime {
    /// Create a new sandboxed Lua runtime with default memory limit.
    pub fn new() -> LuaResult<Self> {
        Self::new_with_limit(LUA_MEMORY_LIMIT)
    }

    /// Create a new sandboxed Lua runtime with a custom memory limit.
    pub fn new_with_limit(limit: usize) -> LuaResult<Self> {
        let lua = Lua::new();
        lua.set_memory_limit(limit)?;
        lua.globals().set("__MEMORY_LIMIT", limit as u64)?;
        super::sandbox::apply_sandbox(&lua)?;
        Ok(Self {
            lua,
            memory_limit: limit,
        })
    }

    /// Get a reference to the underlying Lua state.
    pub fn lua(&self) -> &Lua {
        &self.lua
    }

    /// Load and execute a chunk of Lua code.
    pub fn exec(&self, code: &str) -> LuaResult<()> {
        self.lua.load(code).exec()
    }

    /// Create an isolated environment table for a driver.
    pub fn create_driver_env(&self, driver_name: &str) -> LuaResult<mlua::Table> {
        super::sandbox::create_driver_env(&self.lua, driver_name)
    }

    /// Get used memory in bytes.
    pub fn used_memory(&self) -> usize {
        self.lua.used_memory()
    }

    /// Get the configured memory limit.
    pub fn memory_limit(&self) -> usize {
        self.memory_limit
    }
}
