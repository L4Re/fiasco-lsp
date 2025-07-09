use std::cmp::max;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::File;
use std::io::prelude::*;
use std::io::BufReader;
use std::iter::Iterator;
use std::path::{Path, PathBuf};
use std::{fs, io};

use lazy_static::lazy_static;
use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum PreprocessSection {
    None,
    Interface,
    Implementation,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LineMapping {
    section: PreprocessSection,
    src_line: u32,
    src_end_line: u32, // Exclusive
    dst_file: PathBuf,
    dst_line: u32,
}

impl LineMapping {
    fn contains(&self, line: u32) -> bool {
        line >= self.src_line && line <= self.src_end_line
    }

    fn overlaps(&self, start: u32, end: u32) -> bool {
        // x1 <= y2 && y1 <= x2
        start <= self.src_end_line && self.src_line <= end
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileLineMappings {
    files: Vec<PathBuf>,
    none: Vec<LineMapping>,
    interface: Vec<LineMapping>,
    implementation: Vec<LineMapping>,
    length: u32,
}

impl FileLineMappings {
    fn new() -> FileLineMappings {
        FileLineMappings {
            files: Vec::new(),
            none: Vec::new(),
            interface: Vec::new(),
            implementation: Vec::new(),
            length: 0,
        }
    }

    fn from_mappings(mappings: Vec<LineMapping>) -> FileLineMappings {
        let mut m = Self::new();
        for mapping in mappings {
            m.push(mapping)
        }
        m
    }

    fn access(&mut self, section: PreprocessSection) -> &mut Vec<LineMapping> {
        match section {
            PreprocessSection::None => &mut self.none,
            PreprocessSection::Interface => &mut self.interface,
            PreprocessSection::Implementation => &mut self.implementation,
        }
    }

    fn get(&self, section: PreprocessSection) -> &Vec<LineMapping> {
        match section {
            PreprocessSection::None => &self.none,
            PreprocessSection::Interface => &self.interface,
            PreprocessSection::Implementation => &self.implementation,
        }
    }

    fn push(&mut self, mapping: LineMapping) {
        if mapping.src_end_line > self.length {
            self.length = mapping.src_end_line;
        }
        if !self.files.contains(&mapping.dst_file) {
            self.files.push(mapping.dst_file.clone());
        }
        self.access(mapping.section).push(mapping);
    }

    fn sort(&mut self) {
        self.none.sort_by_key(|l| l.src_line);
        self.interface.sort_by_key(|l| l.src_line);
        self.implementation.sort_by_key(|l| l.src_line);
    }

    fn check(&self) {
        let check_section = |section: &[LineMapping]| {
            section.windows(2).for_each(|w| {
                assert!(
                    w[0].src_end_line <= w[1].src_line,
                    "Ranges overlap: a = {:?}, b = {:?}",
                    w[0],
                    w[1]
                )
            })
        };
        check_section(&self.none);
        check_section(&self.interface);
        check_section(&self.implementation);
    }

    fn length(&self) -> u32 {
        self.length
    }
}

type LineMappings = HashMap<PathBuf, FileLineMappings>;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum MapDirection {
    ToPreprocess,
    FromPreprocess,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FiascoSourceMapping {
    to_preprocess: LineMappings,
    from_preprocess: LineMappings,
}

#[derive(Debug)]
pub struct SourceLocation {
    pub path: PathBuf,
    pub line: u32,
    pub character: u32,
}

impl FiascoSourceMapping {
    fn new() -> FiascoSourceMapping {
        FiascoSourceMapping {
            to_preprocess: LineMappings::new(),
            from_preprocess: LineMappings::new(),
        }
    }

    fn sort(&mut self) {
        for mappings in self.to_preprocess.values_mut() {
            mappings.sort()
        }
        for mappings in self.from_preprocess.values_mut() {
            mappings.sort()
        }
    }

    fn check(&self) {
        for mappings in self.to_preprocess.values() {
            mappings.check()
        }
        for mappings in self.from_preprocess.values() {
            mappings.check()
        }
    }

    fn find_mapping<'a>(
        line_mappings: &'a LineMappings,
        path: &str,
        line: u32,
        section: PreprocessSection,
    ) -> Option<&'a LineMapping> {
        let path = PathBuf::from(path);
        match line_mappings.get(&path) {
            None => None,
            Some(mappings) => {
                debug!("Line {}", line);
                // NOTE: This relies on the assumption that there are no overlapping mappings.
                let index = mappings.get(section).partition_point(|l| line >= l.src_line);
                // If the predicate is never true, this will return the index of the first element,
                // even if that starts after the given line number, so check.
                if index > 0 {
                    let mapping = &mappings.get(section)[index - 1];
                    debug!(
                        "Mapping Src Line {} -> Dst Line {} ({} -> {})",
                        mapping.src_line,
                        mapping.dst_line,
                        path.display(),
                        mapping.dst_file.display()
                    );
                    assert!(line >= mapping.src_line);
                    if mapping.contains(line) {
                        Some(mapping)
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
        }
    }

    fn iter_mappings<'a>(
        line_mappings: &'a LineMappings,
        path: &str,
        start: u32,
        end: u32,
        section: PreprocessSection,
    ) -> impl Iterator<Item = &'a LineMapping> {
        let path = PathBuf::from(path);
        match line_mappings.get(&path) {
            Some(mappings) => mappings.get(section).iter(),
            None => [].iter(),
        }
        .filter(move |mapping| mapping.overlaps(start, end))
    }

    fn get(&self, direction: MapDirection) -> &LineMappings {
        match direction {
            MapDirection::ToPreprocess => &self.to_preprocess,
            MapDirection::FromPreprocess => &self.from_preprocess,
        }
    }

    pub fn map(
        &self,
        direction: MapDirection,
        path: &str,
        line: u32,
        character: u32,
    ) -> SourceLocation {
        let line_mappings = self.get(direction);
        // TODO: Priority to use here? Might depend on use case...
        let mapping =
            Self::find_mapping(line_mappings, path, line, PreprocessSection::Implementation)
                .or_else(|| {
                    Self::find_mapping(line_mappings, path, line, PreprocessSection::Interface)
                })
                .or_else(|| Self::find_mapping(line_mappings, path, line, PreprocessSection::None));
        match mapping {
            None => {
                debug!("No mapping found for Line {} ({})", line, path);
                SourceLocation { path: PathBuf::from(path), line, character }
            }
            Some(mapping) => SourceLocation {
                path: mapping.dst_file.clone(),
                line: mapping.dst_line + (line - mapping.src_line),
                character,
            },
        }
    }

    pub fn map_files(&self, direction: MapDirection, path: &str) -> &[PathBuf] {
        match self.get(direction).get(&PathBuf::from(path)) {
            None => &[],
            Some(mappings) => &mappings.files,
        }
    }

    pub fn map_files_with_range(
        &self,
        direction: MapDirection,
        path: &str,
        start: u32,
        end: u32,
    ) -> HashSet<&Path> {
        let line_mappings = self.get(direction);
        Self::iter_mappings(line_mappings, path, start, end, PreprocessSection::Implementation)
            .chain(Self::iter_mappings(
                line_mappings,
                path,
                start,
                end,
                PreprocessSection::Interface,
            ))
            .chain(Self::iter_mappings(line_mappings, path, start, end, PreprocessSection::None))
            .map(|mapping| mapping.dst_file.as_ref())
            .collect()
    }

    pub fn file_length(&self, direction: MapDirection, path: &Path) -> Option<u32> {
        self.get(direction).get(path).map(FileLineMappings::length)
    }
}

lazy_static! {
    static ref NAME_REPLACE_RE: Regex = Regex::new(r"[+-.]").unwrap();
    static ref LINE_REF_RE: Regex = Regex::new(r#"^#line (\d+) "(.+)"$"#).unwrap();
    static ref INTERFACE_SEC_RE: Regex = Regex::new(r"^// INTERFACE").unwrap();
    static ref IMPLEMENTATION_SEC_RE: Regex = Regex::new(r"^// IMPLEMENTATION").unwrap();
}

fn extract_line_mappings<I>(name: &str, lines: I) -> Vec<LineMapping>
where
    I: IntoIterator<Item = (usize, io::Result<String>)>,
{
    let endif_pattern = format!("#endif // {}", NAME_REPLACE_RE.replace(name, "_"));
    let mut cur_section = PreprocessSection::None;
    let mut mappings: Vec<LineMapping> = Vec::new();
    let mut ln = 0;
    let mut ln_offset = 0;
    for (l, r) in lines {
        ln = l;
        if let Ok(line) = r {
            if let Some(cap) = LINE_REF_RE.captures(line.as_str()) {
                if let Some(m) = mappings.last_mut() {
                    m.src_end_line = max(m.src_line, l as u32 - ln_offset - 1)
                };
                mappings.push(LineMapping {
                    section: cur_section,
                    src_line: l as u32 + 1, // Adjust for the #line comment itself
                    src_end_line: 0,        // Set later
                    dst_file: PathBuf::from(&cap[2]),
                    dst_line: cap[1].parse::<u32>().unwrap_or(1) - 1,
                });
                ln_offset = 0;
            } else if line.starts_with("// INTERFACE") {
                cur_section = PreprocessSection::Interface;
                if let Some(m) = mappings.last_mut() {
                    m.src_end_line = l as u32
                };
                // Do not include the preprocess generated comments into the mapping.
                ln_offset = 5;
            } else if line.starts_with("// IMPLEMENTATION") {
                cur_section = PreprocessSection::Implementation;
                if let Some(m) = mappings.last_mut() {
                    m.src_end_line = l as u32
                };
                // Do not include the preprocess generated comments into the mapping.
                ln_offset = 5;
            } else if line.starts_with("private: // EXTENSION") {
                // Do not include the following three preprocess generated lines into the mapping,
                // to avoid artificial overlaps with other mappings.
                ln_offset = 3;
            } else if line.starts_with(&endif_pattern) {
                // Reached the end of the file, do not include the #endif generated by preprocess
                // into the mapping, to avoid artificial overlaps with other mappings.
                ln -= 1;
                break;
            }
        }
    }
    if let Some(m) = mappings.last_mut() {
        m.src_end_line = ln as u32 - ln_offset
    };
    mappings
}

fn extract_line_mappings_for_file(path: &Path, source_mapping: &mut FiascoSourceMapping) {
    let file = File::open(path);
    if file.is_err() {
        return;
    }

    let reader = BufReader::new(file.unwrap());
    let file_name = path.file_name().and_then(OsStr::to_str).unwrap();
    let mappings = extract_line_mappings(file_name, reader.lines().enumerate());
    for mapping in &mappings {
        if !source_mapping.to_preprocess.contains_key(&mapping.dst_file) {
            source_mapping.to_preprocess.insert(mapping.dst_file.clone(), FileLineMappings::new());
        }
        source_mapping.to_preprocess.get_mut(&mapping.dst_file).unwrap().push(LineMapping {
            section: mapping.section,
            src_line: mapping.dst_line,
            src_end_line: mapping.dst_line + (mapping.src_end_line - mapping.src_line),
            dst_file: path.to_path_buf(),
            dst_line: mapping.src_line,
        })
    }
    source_mapping
        .from_preprocess
        .insert(path.to_path_buf(), FileLineMappings::from_mappings(mappings));
}

lazy_static! {
    static ref STAMP_RE: Regex = Regex::new(r"^auto/stamp-(.+).ready:\s*(.+)$").unwrap();
}

pub fn load_modules(build_dir: &str) -> HashMap<String, Vec<String>> {
    let file = File::open(Path::new(build_dir).join(".Modules.deps")).unwrap();
    let reader = BufReader::new(file);
    reader
        .lines()
        .filter_map(Result::ok)
        .filter_map(|line| {
            STAMP_RE.captures(&line).map(|cap| {
                (cap[1].to_owned(), cap[2].split_whitespace().map(str::to_owned).collect())
            })
        })
        .collect()
}

pub fn load_source_mapping(build_dir: &Path) -> FiascoSourceMapping {
    // let cdb_file = Path::new(json_compilation_db::DEFAULT_FILE_NAME);
    // let entries = json_compilation_db::from_file(cdb_file).unwrap_or(vec![]);

    // TODO: There are also files without specific prefixes...
    let mut source_mapping = FiascoSourceMapping::new();
    let paths = fs::read_dir(build_dir.join("auto")).unwrap();
    for path in paths {
        let p = path.unwrap();
        extract_line_mappings_for_file(&p.path(), &mut source_mapping);
    }
    /*
    for entry in &entries {
        // Preprocessed file are located in the auto directory
        if entry.file.parent().map(|f| f.ends_with("auto")).unwrap_or(false) {
            extract_line_mappings_for_file(&entry.file, &mut source_mapping);
            extract_line_mappings_for_file(&entry.file.with_extension("h"),
                                           &mut source_mapping);
            if let Some(stem) = entry.file.file_stem() {
                let mut file_name = stem.to_owned();
                file_name.push("_i.h");
                extract_line_mappings_for_file(&entry.file.with_file_name(file_name),
                                               &mut source_mapping);
            }
        }
    }
    */
    source_mapping.sort();
    source_mapping.check();
    source_mapping
}

#[cfg(test)]
mod tests {
    use std::io::BufWriter;

    use super::*;

    #[test]
    fn load_mods() {
        let modules = load_modules("/home/george/kk/build/build-fiasco-arm64/auto/");
        env_logger::init();
        println!("{:?}", modules);
    }

    #[test]
    fn load() {
        let source_mapping =
            load_source_mapping(Path::new("/home/george/kk/build/build-fiasco-arm64/auto/"));
        serde_json::to_writer_pretty(
            BufWriter::new(File::create("source_to_preprocess.json").unwrap()),
            &source_mapping.to_preprocess,
        )
        .unwrap();
        serde_json::to_writer_pretty(
            BufWriter::new(File::create("preprocess_to_source.json").unwrap()),
            &source_mapping.from_preprocess,
        )
        .unwrap();

        env_logger::init();
        let mut mapped = source_mapping.map(
            MapDirection::ToPreprocess,
            "/home/george/kk/dev/l4re/fiasco/src/kern/arm/cpu-arm.cpp",
            367,
            10,
        );
        mapped.line += 1;
        mapped.character += 1;
        println!("{:?}", mapped);
    }
}
