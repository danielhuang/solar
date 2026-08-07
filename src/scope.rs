use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;

/// A stack of lexical scopes.
pub struct ScopeStack<V, K = String> {
    scopes: Vec<HashMap<K, V>>,
}

impl<V, K> Default for ScopeStack<V, K> {
    fn default() -> Self {
        ScopeStack { scopes: Vec::new() }
    }
}

impl<V, K: Eq + Hash> ScopeStack<V, K> {
    /// Pushes an empty innermost scope.
    pub fn push(&mut self) {
        self.scopes.push(HashMap::new());
    }

    /// Removes the innermost scope.
    pub fn pop(&mut self) {
        self.scopes.pop();
    }

    /// Defines a name in the innermost scope.
    pub fn define(&mut self, name: K, value: V) {
        self.scopes.last_mut().unwrap().insert(name, value);
    }

    /// Finds the nearest binding for a name.
    pub fn lookup<Q>(&self, name: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
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
    pub fn lookup_at<Q>(&self, name: &Q, index: usize) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.scopes[index].get(name)
    }
}
