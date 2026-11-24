use std::path::{Path, PathBuf};

const ORDER: [&str; 7] = ["saves", "actors", "emitter_changes", "play", "animation", "map", "diagnostics"];

fn rust_files(path: &Path, found: &mut Vec<PathBuf>) {
    if path.is_dir() {
        for entry in std::fs::read_dir(path).unwrap() {
            rust_files(&entry.unwrap().path(), found);
        }
    } else if path.extension().is_some_and(|extension| extension == "rs") {
        found.push(path.to_owned());
    }
}

fn first_name(path: &str) -> String {
    path.chars().take_while(|&c| c.is_alphanumeric() || c == '_').collect()
}

fn crate_names(text: &str) -> Vec<(usize, String)> {
    let mut named = Vec::new();
    for (start, _) in text.match_indices("crate::") {
        let line = text[..start].matches('\n').count() + 1;
        let rest = &text[start + "crate::".len()..];
        let Some(group) = rest.strip_prefix('{') else {
            named.push((line, first_name(rest)));
            continue;
        };
        let (mut depth, mut expecting) = (1, true);
        for (index, c) in group.char_indices() {
            match c {
                '{' => depth += 1,
                '}' if depth == 1 => break,
                '}' => depth -= 1,
                ',' if depth == 1 => expecting = true,
                c if expecting && depth == 1 && (c.is_alphanumeric() || c == '_') => {
                    named.push((line + group[..index].matches('\n').count(), first_name(&group[index..])));
                    expecting = false;
                }
                _ => {}
            }
        }
    }
    named
}

#[test]
fn a_module_names_only_the_modules_before_it() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&source, &mut files);
    let mut wrong = Vec::new();
    for path in &files {
        let relative = path.strip_prefix(&source).unwrap();
        if relative == Path::new("lib.rs") {
            continue;
        }
        let first = relative.components().next().unwrap().as_os_str().to_str().unwrap();
        let module = first.strip_suffix(".rs").unwrap_or(first);
        let Some(place) = ORDER.iter().position(|&name| name == module) else {
            wrong.push(format!("{}: {module} is not in the order", path.display()));
            continue;
        };
        let text = std::fs::read_to_string(path).unwrap();
        for (line, name) in crate_names(&text) {
            if !ORDER[..=place].contains(&name.as_str()) {
                wrong.push(format!("{}:{line}: {module} names crate::{name}", path.display()));
            }
        }
    }
    assert!(wrong.is_empty(), "a module names one after it:\n{}", wrong.join("\n"));
}
