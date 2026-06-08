use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use lopdf::{Dictionary, Document, Object, ObjectId};

#[derive(Debug, Clone)]
pub struct PdfMergeSummary {
    pub output_path: PathBuf,
    pub input_count: usize,
    pub total_pages: usize,
}

pub fn merge_pdfs(input_paths: &[PathBuf], output_path: &Path) -> Result<PdfMergeSummary, String> {
    if input_paths.len() < 2 {
        return Err("Select at least two PDF files to join.".to_string());
    }

    let output_path = output_path.to_path_buf();
    let output_canonical = canonical_for_compare(&output_path);

    for input_path in input_paths {
        if canonical_for_compare(input_path) == output_canonical {
            return Err(format!(
                "Output file cannot be one of the selected source files: {}",
                input_path.display()
            ));
        }
    }

    let mut max_id = 1;
    let mut page_objects: Vec<(ObjectId, Object)> = Vec::new();
    let mut source_objects = BTreeMap::new();

    for input_path in input_paths {
        let mut document = Document::load(input_path)
            .map_err(|error| format!("Could not read {}: {error}", input_path.display()))?;

        document.renumber_objects_with(max_id);
        max_id = document.max_id + 1;

        let pages = document.get_pages();
        if pages.is_empty() {
            return Err(format!("No pages found in {}", input_path.display()));
        }

        for object_id in pages.into_values() {
            let object = page_with_inherited_attributes(&document, object_id, input_path)?;
            page_objects.push((object_id, object));
        }

        source_objects.extend(document.objects);
    }

    let total_pages = page_objects.len();
    let mut output_document = Document::with_version("1.5");

    for (object_id, object) in source_objects {
        match object.type_name().unwrap_or(b"") {
            b"Catalog" | b"Pages" | b"Page" | b"Outlines" | b"Outline" => {}
            _ => {
                output_document.objects.insert(object_id, object);
            }
        }
    }

    output_document.max_id = output_document
        .objects
        .keys()
        .map(|(id, _)| *id)
        .max()
        .unwrap_or(0);
    let pages_id = output_document.new_object_id();
    let catalog_id = output_document.new_object_id();

    for (object_id, object) in &page_objects {
        let dictionary = object.as_dict().map_err(|error| {
            format!("Page object {object_id:?} is not a page dictionary: {error}")
        })?;
        let mut dictionary = dictionary.clone();
        dictionary.set("Parent", pages_id);
        output_document
            .objects
            .insert(*object_id, Object::Dictionary(dictionary));
    }

    let mut pages_dictionary = Dictionary::new();
    pages_dictionary.set("Type", "Pages");
    pages_dictionary.set("Count", total_pages as u32);
    pages_dictionary.set(
        "Kids",
        page_objects
            .iter()
            .map(|(object_id, _)| Object::Reference(*object_id))
            .collect::<Vec<_>>(),
    );
    output_document
        .objects
        .insert(pages_id, Object::Dictionary(pages_dictionary));

    let mut catalog_dictionary = Dictionary::new();
    catalog_dictionary.set("Type", "Catalog");
    catalog_dictionary.set("Pages", pages_id);
    output_document
        .objects
        .insert(catalog_id, Object::Dictionary(catalog_dictionary));
    output_document.trailer.set("Root", catalog_id);
    output_document.max_id = output_document
        .objects
        .keys()
        .map(|(id, _)| *id)
        .max()
        .unwrap_or(0);
    output_document.renumber_objects();

    save_without_partial_output(&mut output_document, &output_path)?;

    Ok(PdfMergeSummary {
        output_path,
        input_count: input_paths.len(),
        total_pages,
    })
}

fn save_without_partial_output(document: &mut Document, output_path: &Path) -> Result<(), String> {
    let parent = output_path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = output_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("joined.pdf");
    let temp_path = parent.join(format!(".printltools-{file_name}.tmp"));

    if temp_path.exists() {
        fs::remove_file(&temp_path).map_err(|error| {
            format!(
                "Could not remove stale temporary file {}: {error}",
                temp_path.display()
            )
        })?;
    }

    if let Err(error) = document.save(&temp_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(format!(
            "Could not write temporary PDF {}: {error}",
            temp_path.display()
        ));
    }

    if output_path.exists() {
        fs::remove_file(output_path).map_err(|error| {
            let _ = fs::remove_file(&temp_path);
            format!(
                "Could not replace existing output file {}: {error}",
                output_path.display()
            )
        })?;
    }

    fs::rename(&temp_path, output_path).map_err(|error| {
        let _ = fs::remove_file(&temp_path);
        format!(
            "Could not move temporary PDF into place at {}: {error}",
            output_path.display()
        )
    })
}

fn page_with_inherited_attributes(
    document: &Document,
    page_id: ObjectId,
    input_path: &Path,
) -> Result<Object, String> {
    let mut dictionary = document
        .get_object(page_id)
        .map_err(|error| {
            format!(
                "Could not read page object in {}: {error}",
                input_path.display()
            )
        })?
        .as_dict()
        .map_err(|error| {
            format!(
                "Page object {page_id:?} in {} is not a page dictionary: {error}",
                input_path.display()
            )
        })?
        .clone();

    for key in INHERITED_PAGE_ATTRIBUTES {
        if !dictionary.has(key) {
            if let Some(value) = inherited_page_attribute(document, &dictionary, key, input_path)? {
                dictionary.set(key.to_vec(), value);
            }
        }
    }

    Ok(Object::Dictionary(dictionary))
}

const INHERITED_PAGE_ATTRIBUTES: [&[u8]; 8] = [
    b"Resources",
    b"MediaBox",
    b"CropBox",
    b"BleedBox",
    b"TrimBox",
    b"ArtBox",
    b"Rotate",
    b"UserUnit",
];

fn inherited_page_attribute(
    document: &Document,
    page_dictionary: &Dictionary,
    key: &[u8],
    input_path: &Path,
) -> Result<Option<Object>, String> {
    let mut parent_id = match page_dictionary
        .get(b"Parent")
        .and_then(Object::as_reference)
    {
        Ok(parent_id) => parent_id,
        Err(_) => return Ok(None),
    };
    let mut seen = HashSet::new();

    loop {
        if !seen.insert(parent_id) {
            return Err(format!(
                "Page tree has a parent cycle in {} near object {parent_id:?}.",
                input_path.display()
            ));
        }

        let parent_dictionary = document.get_dictionary(parent_id).map_err(|error| {
            format!(
                "Could not read page parent object {parent_id:?} in {}: {error}",
                input_path.display()
            )
        })?;

        if let Ok(value) = parent_dictionary.get(key) {
            return Ok(Some(value.clone()));
        }

        parent_id = match parent_dictionary
            .get(b"Parent")
            .and_then(Object::as_reference)
        {
            Ok(parent_id) => parent_id,
            Err(_) => return Ok(None),
        };
    }
}

fn canonical_for_compare(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use lopdf::content::{Content, Operation};
    use lopdf::{Document, Object, Stream, dictionary};

    use super::merge_pdfs;

    #[test]
    fn merges_two_single_page_pdfs() {
        let workspace = TestWorkspace::new();
        let first = workspace.path.join("first.pdf");
        let second = workspace.path.join("second.pdf");
        let output = workspace.path.join("joined.pdf");

        create_test_pdf(&first, "First");
        create_test_pdf(&second, "Second");

        let summary = merge_pdfs(&[first, second], &output).unwrap();

        assert_eq!(summary.input_count, 2);
        assert_eq!(summary.total_pages, 2);
        assert!(output.exists());

        let merged = Document::load(&output).unwrap();
        assert_eq!(merged.get_pages().len(), 2);
    }

    #[test]
    fn preserves_pages_that_inherit_rendering_attributes() {
        let workspace = TestWorkspace::new();
        let first = workspace.path.join("first.pdf");
        let second = workspace.path.join("second.pdf");
        let output = workspace.path.join("joined.pdf");

        create_test_pdf_with_layout(&first, "First", PageLayout::Inherited);
        create_test_pdf_with_layout(&second, "Second", PageLayout::Inherited);

        merge_pdfs(&[first, second], &output).unwrap();

        let merged = Document::load(&output).unwrap();
        let pages = merged.get_pages();
        assert_eq!(pages.len(), 2);

        let mut contents = Vec::new();
        for page_id in pages.into_values() {
            let page = merged.get_dictionary(page_id).unwrap();
            assert!(page.has(b"Resources"));
            assert!(page.has(b"MediaBox"));
            assert!(!merged.get_page_fonts(page_id).unwrap().is_empty());
            contents.push(
                String::from_utf8_lossy(&merged.get_page_content(page_id).unwrap()).into_owned(),
            );
        }

        assert!(contents.iter().any(|content| content.contains("First")));
        assert!(contents.iter().any(|content| content.contains("Second")));
    }

    struct TestWorkspace {
        path: PathBuf,
    }

    impl TestWorkspace {
        fn new() -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "printltools-pdf-test-{}-{stamp}",
                std::process::id()
            ));

            fs::create_dir_all(&path).unwrap();

            Self { path }
        }
    }

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn create_test_pdf(path: &Path, label: &str) {
        create_test_pdf_with_layout(path, label, PageLayout::Direct);
    }

    enum PageLayout {
        Direct,
        Inherited,
    }

    fn create_test_pdf_with_layout(path: &Path, label: &str, layout: PageLayout) {
        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Courier",
        });
        let resources_id = document.add_object(dictionary! {
            "Font" => dictionary! {
                "F1" => font_id,
            },
        });
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 24.into()]),
                Operation::new("Td", vec![100.into(), 600.into()]),
                Operation::new("Tj", vec![Object::string_literal(label)]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id =
            document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let mut page_dictionary = dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        };
        let mut pages_dictionary = dictionary! {
            "Type" => "Pages",
            "Count" => 1,
        };

        match layout {
            PageLayout::Direct => {
                page_dictionary.set("Resources", resources_id);
                page_dictionary.set("MediaBox", vec![0.into(), 0.into(), 595.into(), 842.into()]);
            }
            PageLayout::Inherited => {
                pages_dictionary.set("Resources", resources_id);
                pages_dictionary.set("MediaBox", vec![0.into(), 0.into(), 595.into(), 842.into()]);
            }
        }

        let page_id = document.add_object(page_dictionary);

        pages_dictionary.set("Kids", vec![page_id.into()]);
        document
            .objects
            .insert(pages_id, Object::Dictionary(pages_dictionary));
        let catalog_id = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        document.trailer.set("Root", catalog_id);
        document.save(path).unwrap();
    }
}
