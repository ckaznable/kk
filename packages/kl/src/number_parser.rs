use percent_encoding::percent_decode_str;
use regex::Regex;
use std::path::Path;

pub fn get_number(path: &Path) -> Option<String> {
    let filename = path.file_name()?.to_str()?;
    let decoded_filename = decode_filename_for_number(filename);

    // Check for FC2
    let lower_name = decoded_filename.to_lowercase();
    if lower_name.contains("fc2") {
        // e.g., FC2-PPV-123456 or FC2-123456
        let re = Regex::new(r"(?i)fc2(?:-ppv)?-(\d+)").unwrap();
        if let Some(caps) = re.captures(&decoded_filename) {
            return Some(format!("FC2-{}", &caps[1]));
        }
    }

    let clean_name = clean_filename(&decoded_filename);

    // Try standard format: ALPHA-DIGIT
    let re_std = Regex::new(r"([a-zA-Z]{2,5})[-_]?(\d{3,5})").unwrap();
    if let Some(caps) = re_std.captures(&clean_name) {
        let prefix = &caps[1];
        let num = &caps[2];
        let bad_prefixes = ["fhd", "mp", "sd"];
        if !bad_prefixes.contains(&prefix.to_lowercase().as_str()) {
            return Some(format!("{}-{}", prefix.to_uppercase(), num));
        }
    }

    // Try Tokyo Hot format: n1234
    let re_tokyo = Regex::new(r"(?i)(cz|gedo|k|n|red-|se)(\d{2,4})").unwrap();
    if let Some(caps) = re_tokyo.captures(&clean_name) {
        return Some(format!("{}{}", &caps[1], &caps[2]));
    }

    None
}

fn decode_filename_for_number(name: &str) -> String {
    percent_decode_str(name)
        .decode_utf8()
        .map(|decoded| decoded.into_owned())
        .unwrap_or_else(|_| name.to_string())
}

fn clean_filename(name: &str) -> String {
    let mut s = name.to_string();

    // Remove website prefixes
    let re_site =
        Regex::new(r"^\w+\.(cc|com|net|me|club|jp|tv|xyz|biz|wiki|info|tw|us|de)@").unwrap();
    s = re_site.replace(&s, "").to_string();

    // Remove resolution tags
    let re_tags =
        Regex::new(r"(?i)(-|_)(fhd|hd|sd|1080p|720p|4k|x264|x265|uncensored|hack|leak)").unwrap();
    s = re_tags.replace_all(&s, "").to_string();

    // Remove brackets
    let re_brackets = Regex::new(r"\[.*?\]").unwrap();
    s = re_brackets.replace_all(&s, "").to_string();

    s
}

#[cfg(test)]
mod tests {
    use super::get_number;
    use std::path::Path;

    #[test]
    fn parses_urlencoded_standard_number() {
        assert_eq!(
            get_number(Path::new("SSIS%2D123%20sample.mp4")).as_deref(),
            Some("SSIS-123")
        );
    }

    #[test]
    fn parses_urlencoded_fc2_number() {
        assert_eq!(
            get_number(Path::new("FC2%2DPPV%2D123456%20uncensored.mp4")).as_deref(),
            Some("FC2-123456")
        );
    }

    #[test]
    fn parses_number_after_url_decoding_brackets() {
        assert_eq!(
            get_number(Path::new("%5Bdemo%5DIPZZ%2D221.mkv")).as_deref(),
            Some("IPZZ-221")
        );
    }
}
