//! Just enough XML for WebDAV `PROPFIND` and S3 `ListObjectsV2` replies: find elements by local
//! name (ignoring namespace prefixes) and read their text. Not a general XML parser.

/// The inner text of every `<…:name>` element (any namespace prefix) in `xml`, in order.
pub(crate) fn elements<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = find_open(rest, name) {
        let (tag_end, self_closing) = match rest[start..].find('>') {
            Some(i) => (start + i, rest[start..start + i].ends_with('/')),
            None => break,
        };
        let body_start = tag_end + 1;
        if self_closing {
            out.push("");
            rest = &rest[body_start..];
            continue;
        }
        match find_close(&rest[body_start..], name) {
            Some((end, after)) => {
                out.push(&rest[body_start..body_start + end]);
                rest = &rest[body_start + after..];
            }
            None => break,
        }
    }
    out
}

/// The text of the first `name` element in `xml`, unescaped and trimmed.
pub(crate) fn text(xml: &str, name: &str) -> Option<String> {
    elements(xml, name).first().map(|t| unescape(t.trim()))
}

/// Is there a `name` element at all (e.g. `<d:collection/>`)?
pub(crate) fn has(xml: &str, name: &str) -> bool {
    find_open(xml, name).is_some()
}

/// Position of `<name` or `<prefix:name` followed by `>`, `/` or whitespace.
fn find_open(xml: &str, name: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = xml[from..].find('<') {
        let at = from + i;
        let tag = &xml[at + 1..];
        if !tag.starts_with('/') && !tag.starts_with('?') && !tag.starts_with('!') {
            let end = tag.find(|c: char| c == '>' || c == '/' || c.is_whitespace()).unwrap_or(tag.len());
            let qname = &tag[..end];
            if qname.rsplit(':').next() == Some(name) {
                return Some(at);
            }
        }
        from = at + 1;
    }
    None
}

/// `(start of </…name>, index after its '>')` relative to `xml`.
fn find_close(xml: &str, name: &str) -> Option<(usize, usize)> {
    let mut from = 0;
    while let Some(i) = xml[from..].find("</") {
        let at = from + i;
        let tag = &xml[at + 2..];
        let end = tag.find('>')?;
        if tag[..end].trim().rsplit(':').next() == Some(name) {
            return Some((at, at + 2 + end + 1));
        }
        from = at + 2;
    }
    None
}

pub(crate) fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub(crate) fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&#39;", "'").replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webdav_multistatus() {
        let x = r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:"><d:response><d:href>/dav/a%20b.png</d:href><d:propstat><d:prop><d:getetag>"abc"</d:getetag><d:getcontentlength>12</d:getcontentlength><d:resourcetype/></d:prop></d:propstat></d:response><d:response><d:href>/dav/folder/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response></d:multistatus>"#;
        let rs = elements(x, "response");
        assert_eq!(rs.len(), 2);
        assert_eq!(text(rs[0], "href").as_deref(), Some("/dav/a%20b.png"));
        assert_eq!(text(rs[0], "getetag").as_deref(), Some("\"abc\""));
        assert!(!has(rs[0], "collection"));
        assert!(has(rs[1], "collection"));
    }

    #[test]
    fn s3_listing_without_prefixes() {
        let x = "<ListBucketResult><IsTruncated>true</IsTruncated><Contents><Key>a&amp;b.png</Key><ETag>&quot;0cc175b9c0f1b6a831c399e269772661&quot;</ETag><Size>1</Size></Contents><NextContinuationToken>tok</NextContinuationToken></ListBucketResult>";
        let cs = elements(x, "Contents");
        assert_eq!(text(cs[0], "Key").as_deref(), Some("a&b.png"));
        assert_eq!(text(cs[0], "ETag").as_deref(), Some("\"0cc175b9c0f1b6a831c399e269772661\""));
        assert_eq!(text(x, "NextContinuationToken").as_deref(), Some("tok"));
        // an element name that is a prefix of another doesn't match it
        assert!(text(x, "Content").is_none());
    }
}
