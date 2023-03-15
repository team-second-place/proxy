use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
};

#[derive(Debug)]
pub struct OneToMany<K, V>
where
    K: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
{
    forward: HashMap<K, HashSet<V>>,
    backward: HashMap<V, K>,
}

impl<K, V> Default for OneToMany<K, V>
where
    K: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
{
    fn default() -> Self {
        Self {
            forward: Default::default(),
            backward: Default::default(),
        }
    }
}

impl<K, V> OneToMany<K, V>
where
    K: Clone + Eq + Hash,
    V: Clone + Eq + Hash,
{
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&mut self, key: K, value: V) -> Option<K> {
        self.forward
            .entry(key.clone())
            .or_insert_with(|| HashSet::new())
            .insert(value.clone());

        self.backward
            .insert(value.clone(), key.clone())
            .and_then(|old_key| {
                if old_key != key {
                    let set = self.forward.get_mut(&old_key).unwrap();
                    set.remove(&value);

                    if set.is_empty() {
                        self.forward.remove(&old_key);
                    }

                    Some(old_key)
                } else {
                    None
                }
            })
    }

    pub fn get_from_key(&self, key: &K) -> Option<&HashSet<V>> {
        self.forward.get(key)
    }

    pub fn get_from_value(&self, value: &V) -> Option<&K> {
        self.backward.get(value)
    }

    pub fn remove_key(&mut self, key: &K) -> Option<HashSet<V>> {
        self.forward.remove(key).map(|set| {
            for value in set.iter() {
                self.backward.remove(value);
            }

            set
        })
    }

    pub fn remove_value(&mut self, value: &V) -> Option<K> {
        self.backward.remove(value).map(|key| {
            let set = self.forward.get_mut(&key).unwrap();
            set.remove(value);

            if set.is_empty() {
                self.forward.remove(&key);
            }

            key
        })
    }
}
