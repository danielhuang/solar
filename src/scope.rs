use std::collections::HashMap;

pub struct ScopeStack<V> {
    scopes: Vec<HashMap<String, V>>,
}

impl<V> Default for ScopeStack<V> {
    fn default() -> Self {
        ScopeStack { scopes: Vec::new() }
    }
}

impl<V> ScopeStack<V> {
    pub fn push(&mut self) {
        self.scopes.push(HashMap::new());
    }

    pub fn pop(&mut self) {
        self.scopes.pop();
    }

    pub fn define(&mut self, name: String, value: V) {
        self.scopes.last_mut().unwrap().insert(name, value);
    }

    pub fn lookup(&self, name: &str) -> Option<&V> {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) {
                return Some(v);
            }
        }
        None
    }

    pub fn depth(&self) -> usize {
        self.scopes.len()
    }

    pub fn lookup_at(&self, name: &str, index: usize) -> Option<&V> {
        self.scopes[index].get(name)
    }
}
