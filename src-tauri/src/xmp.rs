//! XMP sidecars that Lightroom, Bridge, Capture One and darktable read.
//!
//! Ratings, colour labels, keywords, caption, creator and copyright are
//! written as standard XMP beside the photo, so leaving the app loses
//! nothing: any of those programs shows the same stars and keywords.
//!
//! Two rules keep it safe to share a sidecar with other programs:
//!
//! - **Read, merge, write.** Only the properties this module owns are
//!   touched; everything else in the file (another program's edits, its
//!   history, namespaces we do not know) is kept byte for byte. The file
//!   is parsed with a namespace-aware reader, so a property is found
//!   whatever prefix the other program chose.
//! - **Whoever changed it last wins.** The sidecar remembers the XMP
//!   file's modification time when we last read or wrote it
//!   (`xmpSeen`). A different time means another program changed it, and
//!   its values are taken; otherwise ours are written.
//!
//! Where the sidecar lives follows Adobe and Capture One: `IMG_1234.xmp`
//! beside a RAW. A JPEG shot alongside a RAW of the same name does not
//! claim that file (it belongs to the RAW); darktable's
//! `IMG_1234.JPG.xmp` is read and updated when that is what exists.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::image_processing::ImageMetadata;
use crate::tagging::{COLOR_TAG_PREFIX, USER_TAG_PREFIX};

pub const NS_RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const NS_XMP: &str = "http://ns.adobe.com/xap/1.0/";
pub const NS_DC: &str = "http://purl.org/dc/elements/1.1/";
pub const NS_LR: &str = "http://ns.adobe.com/lightroom/1.0/";

/// The properties this module reads and writes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct XmpFields {
    /// `xmp:Rating`: 0–5, or −1 for a Lightroom reject.
    pub rating: Option<i32>,
    /// `xmp:Label`: "Red", "Yellow", "Green", "Blue", "Purple"…
    pub label: Option<String>,
    /// `dc:subject`: flat keywords.
    pub keywords: Vec<String>,
    /// `lr:hierarchicalSubject`: "Places|France|Paris".
    pub hierarchical: Vec<String>,
    /// `dc:description`: the caption (EXIF ImageDescription).
    pub description: Option<String>,
    /// `dc:creator`: the author (EXIF Artist).
    pub creator: Vec<String>,
    /// `dc:rights`: the copyright notice.
    pub rights: Option<String>,
}

/// What to change. `None` leaves a property as the file has it;
/// `Some` of an empty value removes it.
#[derive(Debug, Clone, Default)]
pub struct XmpUpdate {
    pub rating: Option<i32>,
    pub label: Option<String>,
    pub keywords: Option<Vec<String>>,
    pub hierarchical: Option<Vec<String>>,
    pub description: Option<String>,
    pub creator: Option<Vec<String>>,
    pub rights: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Prop {
    Rating,
    Label,
    Subject,
    Hierarchical,
    Description,
    Creator,
    Rights,
}

impl Prop {
    const ALL: [Prop; 7] = [
        Prop::Rating,
        Prop::Label,
        Prop::Subject,
        Prop::Hierarchical,
        Prop::Description,
        Prop::Creator,
        Prop::Rights,
    ];

    fn of(ns: Option<&str>, local: &str) -> Option<Prop> {
        match (ns?, local) {
            (NS_XMP, "Rating") => Some(Prop::Rating),
            (NS_XMP, "Label") => Some(Prop::Label),
            (NS_DC, "subject") => Some(Prop::Subject),
            (NS_LR, "hierarchicalSubject") => Some(Prop::Hierarchical),
            (NS_DC, "description") => Some(Prop::Description),
            (NS_DC, "creator") => Some(Prop::Creator),
            (NS_DC, "rights") => Some(Prop::Rights),
            _ => None,
        }
    }

    fn ns(self) -> &'static str {
        match self {
            Prop::Rating | Prop::Label => NS_XMP,
            Prop::Hierarchical => NS_LR,
            _ => NS_DC,
        }
    }

    fn local(self) -> &'static str {
        match self {
            Prop::Rating => "Rating",
            Prop::Label => "Label",
            Prop::Subject => "subject",
            Prop::Hierarchical => "hierarchicalSubject",
            Prop::Description => "description",
            Prop::Creator => "creator",
            Prop::Rights => "rights",
        }
    }
}

impl XmpUpdate {
    fn touches(&self, prop: Prop) -> bool {
        match prop {
            Prop::Rating => self.rating.is_some(),
            Prop::Label => self.label.is_some(),
            Prop::Subject => self.keywords.is_some(),
            Prop::Hierarchical => self.hierarchical.is_some(),
            Prop::Description => self.description.is_some(),
            Prop::Creator => self.creator.is_some(),
            Prop::Rights => self.rights.is_some(),
        }
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Text of a simple or language-alternative property element.
fn element_text(node: roxmltree::Node) -> Option<String> {
    let items = list_items(node);
    if !items.is_empty() {
        // rdf:Alt: prefer the default language.
        let default = node
            .descendants()
            .filter(|n| n.has_tag_name((NS_RDF, "li")))
            .find(|n| {
                n.attribute(("http://www.w3.org/XML/1998/namespace", "lang")) == Some("x-default")
            })
            .and_then(|n| n.text())
            .map(str::to_string);
        return default.or_else(|| items.into_iter().next());
    }
    node.text()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// Items of an rdf:Bag, rdf:Seq or rdf:Alt inside a property element.
fn list_items(node: roxmltree::Node) -> Vec<String> {
    node.descendants()
        .filter(|n| n.has_tag_name((NS_RDF, "li")))
        .filter_map(|n| n.text())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// The owned properties of an XMP document, from every rdf:Description,
/// in either attribute or element form.
pub fn read(content: &str) -> Result<XmpFields, String> {
    let doc = roxmltree::Document::parse(content).map_err(|e| format!("invalid XMP: {e}"))?;
    let mut f = XmpFields::default();
    for desc in doc
        .descendants()
        .filter(|n| n.has_tag_name((NS_RDF, "Description")))
    {
        for a in desc.attributes() {
            let value = a.value().trim().to_string();
            match Prop::of(a.namespace(), a.name()) {
                Some(Prop::Rating) => f.rating = value.parse::<f64>().ok().map(|r| r as i32),
                Some(Prop::Label) if !value.is_empty() => f.label = Some(value),
                Some(Prop::Description) if !value.is_empty() => f.description = Some(value),
                Some(Prop::Rights) if !value.is_empty() => f.rights = Some(value),
                _ => {}
            }
        }
        for child in desc.children().filter(|n| n.is_element()) {
            let name = child.tag_name();
            match Prop::of(name.namespace(), name.name()) {
                Some(Prop::Rating) => {
                    f.rating = element_text(child)
                        .and_then(|t| t.parse::<f64>().ok())
                        .map(|r| r as i32)
                }
                Some(Prop::Label) => f.label = element_text(child),
                Some(Prop::Subject) => f.keywords = list_items(child),
                Some(Prop::Hierarchical) => f.hierarchical = list_items(child),
                Some(Prop::Description) => f.description = element_text(child),
                Some(Prop::Creator) => {
                    let items = list_items(child);
                    f.creator = if items.is_empty() {
                        element_text(child).into_iter().collect()
                    } else {
                        items
                    };
                }
                Some(Prop::Rights) => f.rights = element_text(child),
                None => {}
            }
        }
    }
    Ok(f)
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

const SKELETON: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Darkroom Index">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
"#;

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// Widen `range` back over the whitespace before it, so removing a
/// property does not leave an empty line behind.
fn with_leading_space(content: &str, range: Range<usize>) -> Range<usize> {
    let before = &content[..range.start];
    let trimmed = before.trim_end_matches([' ', '\t', '\r', '\n']);
    trimmed.len()..range.end
}

fn render_list(p: &str, rdf: &str, container: &str, items: &[String]) -> String {
    let mut s = format!("\n   <{p}>\n    <{rdf}:{container}>");
    for item in items {
        s.push_str(&format!("\n     <{rdf}:li>{}</{rdf}:li>", escape(item)));
    }
    s.push_str(&format!("\n    </{rdf}:{container}>\n   </{p}>"));
    s
}

fn render_alt(p: &str, rdf: &str, text: &str) -> String {
    format!(
        "\n   <{p}>\n    <{rdf}:Alt>\n     <{rdf}:li xml:lang=\"x-default\">{}</{rdf}:li>\n    </{rdf}:Alt>\n   </{p}>",
        escape(text)
    )
}

/// Apply `update` to an XMP document (or a new one when `content` is
/// `None`), keeping everything else in it unchanged.
pub fn apply(content: Option<&str>, update: &XmpUpdate) -> Result<String, String> {
    let content = content.unwrap_or(SKELETON);
    let doc = roxmltree::Document::parse(content).map_err(|e| format!("invalid XMP: {e}"))?;
    let descs: Vec<_> = doc
        .descendants()
        .filter(|n| n.has_tag_name((NS_RDF, "Description")))
        .collect();
    let Some(&first) = descs.first() else {
        // No rdf:Description at all: start afresh rather than guess.
        return apply(None, update);
    };

    // (range, replacement), applied back to front.
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();

    // Remove every owned property we are about to write, wherever it is.
    for desc in &descs {
        for a in desc.attributes() {
            if let Some(prop) = Prop::of(a.namespace(), a.name())
                && update.touches(prop)
            {
                edits.push((with_leading_space(content, a.range()), String::new()));
            }
        }
        for child in desc.children().filter(|n| n.is_element()) {
            let name = child.tag_name();
            if let Some(prop) = Prop::of(name.namespace(), name.name())
                && update.touches(prop)
            {
                edits.push((with_leading_space(content, child.range()), String::new()));
            }
        }
    }

    // Prefixes: use the document's own for each namespace, or declare ours.
    let range = first.range();
    let start_tag_end = content[range.start..]
        .find('>')
        .map(|i| range.start + i)
        .ok_or("unterminated rdf:Description")?;
    let self_closing = content[..start_tag_end].ends_with('/');
    let qname_len = content[range.start + 1..]
        .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
        .unwrap_or(0);
    let qname = &content[range.start + 1..range.start + 1 + qname_len];
    let rdf = first.lookup_prefix(NS_RDF).unwrap_or("rdf").to_string();
    let mut declarations = String::new();
    let mut prefixes: HashMap<&'static str, String> = HashMap::new();
    for (ns, wanted) in [(NS_XMP, "xmp"), (NS_DC, "dc"), (NS_LR, "lr")] {
        if !Prop::ALL.iter().any(|p| p.ns() == ns && update.touches(*p)) {
            continue;
        }
        let prefix = match first.lookup_prefix(ns) {
            Some(p) => p.to_string(),
            None => {
                let mut p = wanted.to_string();
                while first.lookup_namespace_uri(Some(&p)).is_some() {
                    p.push('1');
                }
                declarations.push_str(&format!("\n    xmlns:{p}=\"{ns}\""));
                p
            }
        };
        prefixes.insert(ns, prefix);
    }
    if !declarations.is_empty() {
        let at = range.start + 1 + qname_len;
        edits.push((at..at, declarations));
    }

    // The new values, as elements at the end of the first Description.
    let q = |p: Prop| format!("{}:{}", prefixes[p.ns()], p.local());
    let mut body = String::new();
    if let Some(r) = update.rating {
        body.push_str(&format!("\n   <{0}>{r}</{0}>", q(Prop::Rating)));
    }
    if let Some(label) = update.label.as_deref().filter(|l| !l.is_empty()) {
        body.push_str(&format!(
            "\n   <{0}>{1}</{0}>",
            q(Prop::Label),
            escape(label)
        ));
    }
    if let Some(items) = update.keywords.as_ref().filter(|k| !k.is_empty()) {
        body.push_str(&render_list(&q(Prop::Subject), &rdf, "Bag", items));
    }
    if let Some(items) = update.hierarchical.as_ref().filter(|k| !k.is_empty()) {
        body.push_str(&render_list(&q(Prop::Hierarchical), &rdf, "Bag", items));
    }
    if let Some(text) = update.description.as_deref().filter(|t| !t.is_empty()) {
        body.push_str(&render_alt(&q(Prop::Description), &rdf, text));
    }
    if let Some(items) = update.creator.as_ref().filter(|k| !k.is_empty()) {
        body.push_str(&render_list(&q(Prop::Creator), &rdf, "Seq", items));
    }
    if let Some(text) = update.rights.as_deref().filter(|t| !t.is_empty()) {
        body.push_str(&render_alt(&q(Prop::Rights), &rdf, text));
    }
    if !body.is_empty() {
        if self_closing {
            let slash = start_tag_end - 1;
            edits.push((slash..start_tag_end + 1, format!(">{body}\n  </{qname}>")));
        } else {
            let close = content[..range.end]
                .rfind("</")
                .ok_or("rdf:Description has no end tag")?;
            let at = content[..close]
                .trim_end_matches([' ', '\t', '\r', '\n'])
                .len();
            edits.push((at..at, body));
        }
    }

    edits.sort_by(|a, b| b.0.start.cmp(&a.0.start).then(b.0.end.cmp(&a.0.end)));
    let mut out = content.to_string();
    let mut floor = usize::MAX;
    for (r, text) in edits {
        if r.end > floor {
            continue; // overlapping edit (should not happen); keep the file valid
        }
        out.replace_range(r.clone(), &text);
        floor = r.start;
    }
    Ok(out)
}

/// Keep Lightroom's keyword hierarchy in step with a new flat keyword
/// list: paths whose last level is still a keyword stay, the rest go, and
/// keywords no remaining path covers are added at the top level. Without
/// this, Lightroom would bring a removed keyword back from the hierarchy.
pub fn reconcile_hierarchy(existing: &[String], keywords: &[String]) -> Vec<String> {
    let mut kept: Vec<String> = existing
        .iter()
        .filter(|path| {
            path.rsplit('|')
                .next()
                .is_some_and(|leaf| keywords.iter().any(|k| k == leaf))
        })
        .cloned()
        .collect();
    for k in keywords {
        if !kept
            .iter()
            .any(|path| path.split('|').any(|level| level == k))
        {
            kept.push(k.clone());
        }
    }
    kept
}

// ---------------------------------------------------------------------------
// Where the sidecar is
// ---------------------------------------------------------------------------

fn has_raw_sibling(image: &Path) -> bool {
    let (Some(parent), Some(stem)) = (image.parent(), image.file_stem()) else {
        return false;
    };
    let stem = stem.to_string_lossy();
    crate::formats::RAW_EXTENSIONS.iter().any(|(ext, _)| {
        parent.join(format!("{stem}.{ext}")).exists()
            || parent
                .join(format!("{stem}.{}", ext.to_ascii_uppercase()))
                .exists()
    })
}

/// The XMP sidecar for `image` (a real path, not a virtual copy's), and
/// whether it exists. `None` when the photo should not have one of its
/// own: a JPEG whose RAW twin owns `IMG.xmp`.
pub fn sidecar_for(image: &Path) -> Option<(PathBuf, bool)> {
    let is_raw = crate::formats::is_raw_file(image);
    let stem_xmp = image.with_extension("xmp");
    let stem_xmp_upper = image.with_extension("XMP");
    let name = image.file_name()?.to_string_lossy().into_owned();
    let full_xmp = image.with_file_name(format!("{name}.xmp"));
    let full_xmp_upper = image.with_file_name(format!("{name}.XMP"));

    for candidate in [&stem_xmp, &stem_xmp_upper] {
        if candidate.exists() {
            if is_raw || !has_raw_sibling(image) {
                return Some((candidate.clone(), true));
            }
            break;
        }
    }
    for candidate in [&full_xmp, &full_xmp_upper] {
        if candidate.exists() {
            return Some((candidate.clone(), true));
        }
    }
    if is_raw || !has_raw_sibling(image) {
        Some((stem_xmp, false))
    } else {
        None
    }
}

fn modified_ms(path: &Path) -> Option<u64> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(t.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64)
}

// ---------------------------------------------------------------------------
// Sync with the app's sidecar
// ---------------------------------------------------------------------------

/// "red" ↔ "Red", as Lightroom writes labels.
fn label_from_color(color: &str) -> String {
    let mut c = color.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// The app's view of the owned fields, from its sidecar.
fn fields_from_metadata(m: &ImageMetadata) -> XmpFields {
    let tags = m.tags.clone().unwrap_or_default();
    let label = tags
        .iter()
        .find_map(|t| t.strip_prefix(COLOR_TAG_PREFIX))
        .map(label_from_color);
    // Only keywords a person gave are shared. AI tags are guesses that
    // stay in the app until accepted; colour is the label.
    let keywords = tags
        .iter()
        .filter_map(|t| t.strip_prefix(USER_TAG_PREFIX))
        .map(str::to_string)
        .collect();
    let exif = m.exif.clone().unwrap_or_default();
    let text = |k: &str| {
        exif.get(k)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    XmpFields {
        // Lightroom writes a rejected photo as rating -1; picks live only
        // in its catalog, so they stay in the app's sidecar.
        rating: Some(if m.flag.as_deref() == Some("reject") {
            -1
        } else {
            m.rating as i32
        }),
        label,
        keywords,
        hierarchical: Vec::new(),
        description: text("ImageDescription"),
        creator: text("Artist").into_iter().collect(),
        rights: text("Copyright"),
    }
}

/// Put XMP values into the app's sidecar. `merge` (the first time a file
/// is seen) keeps what the app already has and adds to it; otherwise the
/// XMP values replace the app's.
fn apply_to_metadata(m: &mut ImageMetadata, x: &XmpFields, merge: bool) {
    if x.rating == Some(-1) {
        m.flag = Some("reject".into());
    } else if !merge && m.flag.as_deref() == Some("reject") && x.rating.is_some() {
        // Un-rejected in another program.
        m.flag = None;
    }
    if let Some(r) = x.rating.filter(|r| (0..=5).contains(r))
        && (!merge || m.rating == 0)
    {
        m.rating = r as u8;
        if let Some(obj) = m.adjustments.as_object_mut() {
            obj.insert("rating".into(), serde_json::json!(r));
        }
    }

    let mut tags = m.tags.clone().unwrap_or_default();
    let had_color = tags.iter().any(|t| t.starts_with(COLOR_TAG_PREFIX));
    if !merge || !had_color {
        tags.retain(|t| !t.starts_with(COLOR_TAG_PREFIX));
        if let Some(label) = x.label.as_deref().filter(|l| !l.is_empty()) {
            tags.push(format!("{COLOR_TAG_PREFIX}{}", label.to_lowercase()));
        }
    }
    // Keywords become the app's own tags (`user:`), so clearing AI tags
    // never removes them. Older versions wrote `user:` into XMP; strip it.
    let incoming: Vec<String> = x
        .keywords
        .iter()
        .map(|k| k.strip_prefix(USER_TAG_PREFIX).unwrap_or(k).to_string())
        .filter(|k| !k.is_empty())
        .collect();
    if !merge {
        tags.retain(|t| !t.starts_with(USER_TAG_PREFIX));
    }
    for k in incoming {
        let tag = format!("{USER_TAG_PREFIX}{k}");
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    m.tags = if tags.is_empty() { None } else { Some(tags) };

    let exif = m.exif.get_or_insert_with(HashMap::new);
    let mut put = |key: &str, value: Option<String>| {
        if let Some(v) = value.filter(|v| !v.is_empty())
            && (!merge || !exif.contains_key(key))
        {
            exif.insert(key.to_string(), v);
        }
    };
    put("ImageDescription", x.description.clone());
    put(
        "Artist",
        (!x.creator.is_empty()).then(|| x.creator.join("; ")),
    );
    put("Copyright", x.rights.clone());
    if m.exif.as_ref().is_some_and(|e| e.is_empty()) {
        m.exif = None;
    }
}

fn is_virtual(path: &str) -> bool {
    path.contains("?vc=")
}

/// Take in another program's changes to `path`'s XMP. Returns whether
/// `metadata` changed (the caller saves the sidecar).
pub fn pull(path: &str, metadata: &mut ImageMetadata) -> bool {
    if is_virtual(path) {
        return false;
    }
    let image = Path::new(path);
    let Some((xmp, true)) = sidecar_for(image) else {
        return false;
    };
    let Some(mtime) = modified_ms(&xmp) else {
        return false;
    };
    if metadata.xmp_seen == Some(mtime) {
        return false;
    }
    let fields = match std::fs::read_to_string(&xmp)
        .map_err(|e| e.to_string())
        .and_then(|c| read(&c))
    {
        Ok(f) => f,
        Err(e) => {
            log::warn!("Ignoring unreadable XMP {xmp:?}: {e}");
            return false;
        }
    };
    let merge = metadata.xmp_seen.is_none();
    let color = |m: &ImageMetadata| {
        m.tags
            .iter()
            .flatten()
            .find_map(|t| t.strip_prefix(COLOR_TAG_PREFIX).map(str::to_string))
    };
    let color_before = color(metadata);
    apply_to_metadata(metadata, &fields, merge);
    metadata.xmp_seen = Some(mtime);
    let color_after = color(metadata);
    if color_after != color_before {
        crate::finder_tags::set_color_label(image, color_after.as_deref());
    }
    true
}

/// Write the app's values for `path` into its XMP, creating the file only
/// when `create_if_missing` and there is something worth sharing. Updates
/// `metadata.xmp_seen`; the caller saves the sidecar afterwards.
pub fn push(path: &str, metadata: &mut ImageMetadata, create_if_missing: bool) {
    if is_virtual(path) {
        return;
    }
    let image = Path::new(path);
    let Some((xmp, exists)) = sidecar_for(image) else {
        return;
    };
    let mine = fields_from_metadata(metadata);
    let existing = if exists {
        match std::fs::read_to_string(&xmp) {
            Ok(c) => Some(c),
            Err(e) => {
                log::warn!("Could not read XMP {xmp:?}: {e}");
                return;
            }
        }
    } else {
        // Stars, or a reject (-1), are worth sharing.
        let worth_it = mine.rating.unwrap_or(0) != 0
            || mine.label.is_some()
            || !mine.keywords.is_empty()
            || mine.description.is_some()
            || !mine.creator.is_empty()
            || mine.rights.is_some();
        if !create_if_missing || !worth_it {
            return;
        }
        None
    };

    let theirs = existing
        .as_deref()
        .and_then(|c| read(c).ok())
        .unwrap_or_default();
    let keywords_changed = theirs.keywords != mine.keywords;
    let update = XmpUpdate {
        rating: Some(mine.rating.unwrap_or(0)),
        label: Some(mine.label.clone().unwrap_or_default()),
        keywords: keywords_changed.then(|| mine.keywords.clone()),
        hierarchical: keywords_changed
            .then(|| reconcile_hierarchy(&theirs.hierarchical, &mine.keywords)),
        // Text fields only when the app has them, so a caption written in
        // another program is never erased by an app that never set one.
        description: mine.description.clone(),
        creator: (!mine.creator.is_empty()).then(|| mine.creator.clone()),
        rights: mine.rights.clone(),
    };
    let unchanged = exists
        && theirs.rating.unwrap_or(0) == mine.rating.unwrap_or(0)
        && theirs.label == mine.label
        && !keywords_changed
        && (mine.description.is_none() || theirs.description == mine.description)
        && (mine.creator.is_empty() || theirs.creator == mine.creator)
        && (mine.rights.is_none() || theirs.rights == mine.rights);
    if unchanged {
        metadata.xmp_seen = modified_ms(&xmp);
        return;
    }

    let written = match apply(existing.as_deref(), &update) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("Not updating XMP {xmp:?}: {e}");
            return;
        }
    };
    // Write beside it and rename, so a crash never leaves half a file.
    let tmp = xmp.with_extension("xmp.rapidraw-tmp");
    crate::watcher::note_own_write(&xmp);
    if let Err(e) = std::fs::write(&tmp, written).and_then(|_| std::fs::rename(&tmp, &xmp)) {
        log::warn!("Could not write XMP {xmp:?}: {e}");
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    metadata.xmp_seen = modified_ms(&xmp);
}

#[cfg(test)]
mod tests {
    use super::*;

    const DARKTABLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="XMP Core 4.4.0-Exiv2">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:exif="http://ns.adobe.com/exif/1.0/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:darktable="http://darktable.sf.net/"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:lr="http://ns.adobe.com/lightroom/1.0/"
   exif:DateTimeOriginal="2026:08:10 09:33:16.031"
   xmp:Rating="0"
   darktable:history_end="0">
   <darktable:history>
    <rdf:Seq/>
   </darktable:history>
   <dc:subject>
    <rdf:Bag>
     <rdf:li>darktable</rdf:li>
     <rdf:li>format</rdf:li>
     <rdf:li>jpg</rdf:li>
    </rdf:Bag>
   </dc:subject>
   <lr:hierarchicalSubject>
    <rdf:Bag>
     <rdf:li>darktable|format|jpg</rdf:li>
    </rdf:Bag>
   </lr:hierarchicalSubject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
"#;

    #[test]
    fn reads_attribute_and_element_forms() {
        let f = read(DARKTABLE).unwrap();
        assert_eq!(f.rating, Some(0));
        assert_eq!(f.keywords, ["darktable", "format", "jpg"]);
        assert_eq!(f.hierarchical, ["darktable|format|jpg"]);
        assert_eq!(f.label, None);
    }

    #[test]
    fn writing_keeps_everything_it_does_not_own() {
        let update = XmpUpdate {
            rating: Some(4),
            label: Some("Red".into()),
            keywords: Some(vec!["Paris".into(), "Rock & Roll".into()]),
            hierarchical: Some(vec!["Places|Paris".into(), "Rock & Roll".into()]),
            ..Default::default()
        };
        let out = apply(Some(DARKTABLE), &update).unwrap();
        // Other programs' data is untouched.
        assert!(out.contains(r#"exif:DateTimeOriginal="2026:08:10 09:33:16.031""#));
        assert!(out.contains("darktable:history_end=\"0\""));
        assert!(out.contains("<darktable:history>"));
        // Ours is replaced, escaped, and readable back.
        assert!(!out.contains("xmp:Rating=\"0\""));
        assert!(out.contains("Rock &amp; Roll"));
        let back = read(&out).unwrap();
        assert_eq!(back.rating, Some(4));
        assert_eq!(back.label.as_deref(), Some("Red"));
        assert_eq!(back.keywords, ["Paris", "Rock & Roll"]);
        assert_eq!(back.hierarchical, ["Places|Paris", "Rock & Roll"]);
        // Writing the same again changes nothing.
        assert_eq!(apply(Some(&out), &update).unwrap(), out);
    }

    #[test]
    fn a_new_file_declares_what_it_uses() {
        let update = XmpUpdate {
            rating: Some(3),
            description: Some("Sunset <over> the bay".into()),
            creator: Some(vec!["Chris Manning".into()]),
            rights: Some("© 2026".into()),
            ..Default::default()
        };
        let out = apply(None, &update).unwrap();
        let back = read(&out).unwrap();
        assert_eq!(back.rating, Some(3));
        assert_eq!(back.description.as_deref(), Some("Sunset <over> the bay"));
        assert_eq!(back.creator, ["Chris Manning"]);
        assert_eq!(back.rights.as_deref(), Some("© 2026"));
        assert!(out.contains("xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\""));
        assert!(out.contains("xmlns:dc=\"http://purl.org/dc/elements/1.1/\""));
    }

    #[test]
    fn other_prefixes_for_our_namespaces_are_respected() {
        let doc = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><r:RDF xmlns:r="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<r:Description r:about="" xmlns:a="http://ns.adobe.com/xap/1.0/" a:Rating="2" a:Label="Blue"/>
</r:RDF></x:xmpmeta>"#;
        assert_eq!(read(doc).unwrap().rating, Some(2));
        let out = apply(
            Some(doc),
            &XmpUpdate {
                rating: Some(5),
                label: Some(String::new()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(out.contains("<a:Rating>5</a:Rating>"), "{out}");
        assert!(out.contains("</r:Description>"));
        let back = read(&out).unwrap();
        assert_eq!(back.rating, Some(5));
        assert_eq!(back.label, None);
    }

    #[test]
    fn hierarchy_follows_the_keywords() {
        let existing = vec![
            "darktable|format|jpg".to_string(),
            "Places|Paris".to_string(),
        ];
        let kw = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            reconcile_hierarchy(&existing, &kw(&["darktable", "format", "jpg", "Paris"])),
            existing
        );
        assert_eq!(
            reconcile_hierarchy(&existing, &kw(&["Paris", "Beach"])),
            kw(&["Places|Paris", "Beach"])
        );
    }

    #[test]
    fn sync_round_trip_and_external_changes() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("DSC1.ARW");
        let jpg = dir.path().join("DSC1.JPG");
        std::fs::write(&raw, b"raw").unwrap();
        std::fs::write(&jpg, b"jpg").unwrap();
        let raw_s = raw.to_string_lossy().to_string();

        // The JPEG twin does not get the RAW's sidecar.
        assert!(sidecar_for(&jpg).is_none());
        assert_eq!(
            sidecar_for(&raw),
            Some((dir.path().join("DSC1.xmp"), false))
        );

        let mut m = ImageMetadata {
            rating: 4,
            tags: Some(vec![
                "color:red".into(),
                "user:Paris".into(),
                "beach".into(), // an AI tag: not shared
            ]),
            ..Default::default()
        };
        push(&raw_s, &mut m, false);
        assert!(
            !dir.path().join("DSC1.xmp").exists(),
            "not created unless asked"
        );
        push(&raw_s, &mut m, true);
        let xmp = dir.path().join("DSC1.xmp");
        let written = read(&std::fs::read_to_string(&xmp).unwrap()).unwrap();
        assert_eq!(written.rating, Some(4));
        assert_eq!(written.label.as_deref(), Some("Red"));
        assert_eq!(written.keywords, ["Paris"]);
        assert!(m.xmp_seen.is_some());

        // Nothing changed elsewhere: pulling does nothing.
        assert!(!pull(&raw_s, &mut m));

        // Another program changes it.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let theirs = apply(
            Some(&std::fs::read_to_string(&xmp).unwrap()),
            &XmpUpdate {
                rating: Some(2),
                label: Some("Green".into()),
                keywords: Some(vec!["Paris".into(), "Night".into()]),
                ..Default::default()
            },
        )
        .unwrap();
        std::fs::write(&xmp, theirs).unwrap();
        assert!(pull(&raw_s, &mut m));
        assert_eq!(m.rating, 2);
        let tags = m.tags.clone().unwrap();
        assert!(tags.contains(&"color:green".to_string()));
        assert!(!tags.contains(&"color:red".to_string()));
        assert!(tags.contains(&"user:Night".to_string()));
        assert!(tags.contains(&"beach".to_string()), "AI tags are kept");

        // Virtual copies never touch the master's XMP.
        let mut vc = m.clone();
        vc.rating = 5;
        let before = std::fs::read_to_string(&xmp).unwrap();
        push(&format!("{raw_s}?vc=abc123"), &mut vc, true);
        assert_eq!(std::fs::read_to_string(&xmp).unwrap(), before);
    }

    #[test]
    fn rejects_travel_as_lightroom_writes_them() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("DSC2.ARW");
        std::fs::write(&raw, b"raw").unwrap();
        let raw_s = raw.to_string_lossy().to_string();
        let xmp = dir.path().join("DSC2.xmp");

        // Rejected in the app: XMP says -1, the star rating is kept in the app.
        let mut m = ImageMetadata {
            rating: 3,
            flag: Some("reject".into()),
            ..Default::default()
        };
        push(&raw_s, &mut m, true);
        assert_eq!(
            read(&std::fs::read_to_string(&xmp).unwrap())
                .unwrap()
                .rating,
            Some(-1)
        );
        assert_eq!(m.rating, 3);

        // Lightroom un-rejects it and gives it 4 stars.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let theirs = apply(
            Some(&std::fs::read_to_string(&xmp).unwrap()),
            &XmpUpdate {
                rating: Some(4),
                ..Default::default()
            },
        )
        .unwrap();
        std::fs::write(&xmp, theirs).unwrap();
        assert!(pull(&raw_s, &mut m));
        assert_eq!(m.flag, None);
        assert_eq!(m.rating, 4);

        // And rejects it again.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let theirs = apply(
            Some(&std::fs::read_to_string(&xmp).unwrap()),
            &XmpUpdate {
                rating: Some(-1),
                ..Default::default()
            },
        )
        .unwrap();
        std::fs::write(&xmp, theirs).unwrap();
        assert!(pull(&raw_s, &mut m));
        assert_eq!(m.flag.as_deref(), Some("reject"));
        assert_eq!(m.rating, 4, "a reject does not erase the stars");
    }

    #[test]
    fn first_contact_merges_instead_of_replacing() {
        let dir = tempfile::tempdir().unwrap();
        let jpg = dir.path().join("DSC08202.JPG");
        std::fs::write(&jpg, b"jpg").unwrap();
        std::fs::write(dir.path().join("DSC08202.JPG.xmp"), DARKTABLE).unwrap();
        let mut m = ImageMetadata {
            rating: 3,
            tags: Some(vec!["user:Mine".into()]),
            ..Default::default()
        };
        assert!(pull(&jpg.to_string_lossy(), &mut m));
        assert_eq!(m.rating, 3, "the app's rating stands over an XMP 0");
        let tags = m.tags.unwrap();
        for t in ["user:Mine", "user:darktable", "user:format", "user:jpg"] {
            assert!(tags.contains(&t.to_string()), "{t} in {tags:?}");
        }
    }
}
