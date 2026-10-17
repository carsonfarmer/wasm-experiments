//! Helpers for columnar batches: string columns in Arrow's UTF-8 layout
//! (`data` + `offsets`, value `i` is `data[offsets[i]..offsets[i + 1]]`).

/// Builds one string column, appending values without per-value allocation.
pub struct StrColBuilder {
    data: String,
    offsets: Vec<u32>,
}

impl Default for StrColBuilder {
    fn default() -> Self {
        Self::with_capacity(0, 0)
    }
}

impl StrColBuilder {
    pub fn with_capacity(rows: usize, bytes: usize) -> Self {
        let mut offsets = Vec::with_capacity(rows + 1);
        offsets.push(0);
        StrColBuilder { data: String::with_capacity(bytes), offsets }
    }

    pub fn push(&mut self, s: &str) {
        self.data.push_str(s);
        self.offsets.push(self.data.len() as u32);
    }

    pub fn len(&self) -> usize {
        self.offsets.len() - 1
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Take the column's `(data, offsets)`, leaving the builder empty but
    /// with the same capacity reserved for the next batch.
    pub fn finish(&mut self) -> (String, Vec<u32>) {
        let rows = self.len();
        let cap = self.data.capacity();
        let data = std::mem::replace(&mut self.data, String::with_capacity(cap));
        let mut fresh = Vec::with_capacity(rows + 1);
        fresh.push(0);
        let offsets = std::mem::replace(&mut self.offsets, fresh);
        (data, offsets)
    }
}

/// Value `i` of a string column (empty if the offsets are malformed).
#[inline]
pub fn get<'a>(data: &'a str, offsets: &[u32], i: usize) -> &'a str {
    match (offsets.get(i), offsets.get(i + 1)) {
        (Some(&a), Some(&b)) => data.get(a as usize..b as usize).unwrap_or(""),
        _ => "",
    }
}

/// Number of rows in a string column.
#[inline]
pub fn rows(offsets: &[u32]) -> usize {
    offsets.len().saturating_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut b = StrColBuilder::default();
        for s in ["a", "", "héllo", "xyz"] {
            b.push(s);
        }
        assert_eq!(b.len(), 4);
        let (data, offsets) = b.finish();
        assert_eq!(rows(&offsets), 4);
        let got: Vec<&str> = (0..4).map(|i| get(&data, &offsets, i)).collect();
        assert_eq!(got, ["a", "", "héllo", "xyz"]);
        assert!(b.is_empty());
        b.push("again");
        let (data, offsets) = b.finish();
        assert_eq!(get(&data, &offsets, 0), "again");
    }

    #[test]
    fn malformed_offsets_are_empty() {
        assert_eq!(get("abc", &[0, 9], 0), "");
        assert_eq!(get("abc", &[0], 0), "");
        assert_eq!(get("héllo", &[0, 2], 0), ""); // not a char boundary
    }
}
