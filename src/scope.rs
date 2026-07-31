use std::collections::HashMap;

/// A stack of lexical scopes.
pub struct ScopeStack<V> {
    scopes: Vec<HashMap<String, V>>,
}

impl<V> Default for ScopeStack<V> {
    fn default() -> Self {
        ScopeStack { scopes: Vec::new() }
    }
}

impl<V> ScopeStack<V> {
    /// Pushes an empty innermost scope.
    pub fn push(&mut self) {
        self.scopes.push(HashMap::new());
    }

    /// Removes the innermost scope.
    pub fn pop(&mut self) {
        self.scopes.pop();
    }

    /// Defines a name in the innermost scope.
    pub fn define(&mut self, name: String, value: V) {
        self.scopes.last_mut().unwrap().insert(name, value);
    }

    /// Finds the nearest binding for a name.
    pub fn lookup(&self, name: &str) -> Option<&V> {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) {
                return Some(v);
            }
        }
        None
    }

    /// Returns the number of active scopes.
    pub fn depth(&self) -> usize {
        self.scopes.len()
    }

    /// Looks up a name in a specific scope.
    pub fn lookup_at(&self, name: &str, index: usize) -> Option<&V> {
        self.scopes[index].get(name)
    }
}
