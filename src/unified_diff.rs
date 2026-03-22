#[derive(Clone, Debug, Default)]
pub struct Document {
    pub items: Vec<Item>,
}

#[derive(Clone, Debug)]
pub enum Item {
    FileHeader(String),
    Meta(String),
    Hunk(Hunk),
}

#[derive(Clone, Debug)]
pub struct Hunk {
    pub old_start: usize,
    pub new_start: usize,
    pub new_len: usize,
    pub rows: Vec<Row>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Row {
    Context(String),
    Delete(String),
    Insert(String),
    Change { old: String, new: String },
    Annotation(String),
}

enum RawRow {
    Context(String),
    Delete(String),
    Insert(String),
    Annotation(String),
}

pub fn parse(input: &str) -> Document {
    let mut items = Vec::new();
    let mut current_header: Option<String> = None;
    let mut raw_rows: Vec<RawRow> = Vec::new();

    for line in input.lines() {
        if let Some(path) = parse_file_header(line) {
            flush_hunk(&mut items, &mut current_header, &mut raw_rows);
            items.push(Item::FileHeader(path));
            continue;
        }

        if line.starts_with("@@") {
            flush_hunk(&mut items, &mut current_header, &mut raw_rows);
            current_header = Some(line.to_owned());
            continue;
        }

        if current_header.is_some() {
            match line.chars().next() {
                Some(' ') => {
                    raw_rows.push(RawRow::Context(line[1..].to_owned()));
                    continue;
                }
                Some('-') => {
                    raw_rows.push(RawRow::Delete(line[1..].to_owned()));
                    continue;
                }
                Some('+') => {
                    raw_rows.push(RawRow::Insert(line[1..].to_owned()));
                    continue;
                }
                Some('\\') => {
                    raw_rows.push(RawRow::Annotation(line.to_owned()));
                    continue;
                }
                _ => {
                    flush_hunk(&mut items, &mut current_header, &mut raw_rows);
                }
            }
        }

        if is_redundant_path_meta(line) {
            continue;
        }

        items.push(Item::Meta(line.to_owned()));
    }

    flush_hunk(&mut items, &mut current_header, &mut raw_rows);

    Document { items }
}

impl Document {
    pub fn file_paths(&self) -> Vec<String> {
        self.items
            .iter()
            .filter_map(|item| match item {
                Item::FileHeader(path) => Some(path.clone()),
                Item::Meta(_) | Item::Hunk(_) => None,
            })
            .collect()
    }

    pub fn filter_files(&self, query: &str) -> Document {
        if query.is_empty() {
            return self.clone();
        }

        let mut items = Vec::new();
        let mut section = Vec::new();
        let mut matched_file_sections = 0usize;

        for item in &self.items {
            match item {
                Item::FileHeader(path) => {
                    matched_file_sections +=
                        flush_filtered_section(&mut items, &mut section, query);
                    section.push(Item::FileHeader(path.clone()));
                }
                Item::Meta(line) => {
                    if section.is_empty() {
                        continue;
                    }
                    section.push(Item::Meta(line.clone()));
                }
                Item::Hunk(hunk) => {
                    if section.is_empty() {
                        continue;
                    }
                    section.push(Item::Hunk(hunk.clone()));
                }
            }
        }

        matched_file_sections += flush_filtered_section(&mut items, &mut section, query);

        if matched_file_sections == 0 {
            return Document::default();
        }

        Document { items }
    }
}

fn flush_filtered_section(items: &mut Vec<Item>, section: &mut Vec<Item>, query: &str) -> usize {
    let Some(Item::FileHeader(path)) = section.first() else {
        section.clear();
        return 0;
    };

    if path.contains(query) {
        items.extend(section.drain(..));
        1
    } else {
        section.clear();
        0
    }
}

fn parse_file_header(line: &str) -> Option<String> {
    if let Some(rest) = line.strip_prefix("diff --git ") {
        let mut parts = rest.split_whitespace();
        let _old = parts.next()?;
        let new = parts.next()?;
        return Some(strip_diff_path_prefix(new));
    }

    if let Some(rest) = line.strip_prefix("diff -r ") {
        let path = rest.split_whitespace().last()?;
        return Some(strip_diff_path_prefix(path));
    }

    None
}

fn strip_diff_path_prefix(path: &str) -> String {
    let trimmed = path.trim_matches('"');
    trimmed
        .strip_prefix("a/")
        .or_else(|| trimmed.strip_prefix("b/"))
        .unwrap_or(trimmed)
        .to_owned()
}

fn is_redundant_path_meta(line: &str) -> bool {
    line.starts_with("--- ") || line.starts_with("+++ ") || line.starts_with("index ")
}

fn flush_hunk(items: &mut Vec<Item>, header: &mut Option<String>, raw_rows: &mut Vec<RawRow>) {
    let Some(header) = header.take() else {
        raw_rows.clear();
        return;
    };

    let (old_start, new_start, new_len) = parse_hunk_header(&header).unwrap_or((0, 0, 0));
    let rows = build_rows(std::mem::take(raw_rows));
    items.push(Item::Hunk(Hunk {
        old_start,
        new_start,
        new_len,
        rows,
    }));
}

fn parse_hunk_header(header: &str) -> Option<(usize, usize, usize)> {
    let inner = header.strip_prefix("@@ ")?;
    let inner = inner.split(" @@").next()?;
    let mut parts = inner.split_whitespace();
    let old = parts.next()?;
    let new = parts.next()?;
    let (old_start, _) = parse_range(old)?;
    let (new_start, new_len) = parse_range(new)?;
    Some((old_start, new_start, new_len))
}

fn parse_range(value: &str) -> Option<(usize, usize)> {
    let value = value.strip_prefix(['-', '+'])?;
    let mut parts = value.split(',');
    let start: usize = parts.next()?.parse().ok()?;
    let len = parts
        .next()
        .map_or(Some(1usize), |part| part.parse().ok())?;
    Some((start, len))
}

fn build_rows(raw_rows: Vec<RawRow>) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut deleted = Vec::new();
    let mut inserted = Vec::new();

    let flush_change_block =
        |rows: &mut Vec<Row>, deleted: &mut Vec<String>, inserted: &mut Vec<String>| {
            let len = deleted.len().max(inserted.len());
            for index in 0..len {
                match (deleted.get(index), inserted.get(index)) {
                    (Some(old), Some(new)) => rows.push(Row::Change {
                        old: old.clone(),
                        new: new.clone(),
                    }),
                    (Some(old), None) => rows.push(Row::Delete(old.clone())),
                    (None, Some(new)) => rows.push(Row::Insert(new.clone())),
                    (None, None) => {}
                }
            }

            deleted.clear();
            inserted.clear();
        };

    for row in raw_rows {
        match row {
            RawRow::Context(line) => {
                flush_change_block(&mut rows, &mut deleted, &mut inserted);
                rows.push(Row::Context(line));
            }
            RawRow::Delete(line) => deleted.push(line),
            RawRow::Insert(line) => inserted.push(line),
            RawRow::Annotation(line) => {
                flush_change_block(&mut rows, &mut deleted, &mut inserted);
                rows.push(Row::Annotation(line));
            }
        }
    }

    flush_change_block(&mut rows, &mut deleted, &mut inserted);
    rows
}

#[cfg(test)]
mod tests {
    use super::Item;
    use super::Row;
    use super::parse;

    #[test]
    fn parses_context_and_change_rows() {
        let input = "\
diff --git a/a.txt b/a.txt
@@ -1,2 +1,2 @@
-old
+new
 same
";

        let doc = parse(input);
        assert_eq!(doc.items.len(), 2);
        assert!(matches!(&doc.items[0], Item::FileHeader(name) if name == "a.txt"));

        let Item::Hunk(hunk) = &doc.items[1] else {
            panic!("expected hunk");
        };

        assert_eq!(hunk.old_start, 1);
        assert_eq!(hunk.new_start, 1);
        assert_eq!(hunk.new_len, 2);
        assert_eq!(
            hunk.rows,
            vec![
                Row::Change {
                    old: "old".into(),
                    new: "new".into(),
                },
                Row::Context("same".into()),
            ]
        );
    }

    #[test]
    fn keeps_annotations_separate() {
        let input = "\
@@ -1 +1 @@
-before
+after
\\ No newline at end of file
";

        let doc = parse(input);
        let Item::Hunk(hunk) = &doc.items[0] else {
            panic!("expected hunk");
        };

        assert_eq!(
            hunk.rows,
            vec![
                Row::Change {
                    old: "before".into(),
                    new: "after".into(),
                },
                Row::Annotation("\\ No newline at end of file".into()),
            ]
        );
    }

    #[test]
    fn parses_mercurial_file_headers() {
        let input = "\
diff -r 1234abcd -r abcd1234 path/to/demo.txt
@@ -3 +3 @@
-before
+after
";

        let doc = parse(input);
        assert!(matches!(
            &doc.items[0],
            Item::FileHeader(name) if name == "path/to/demo.txt"
        ));
    }
}
