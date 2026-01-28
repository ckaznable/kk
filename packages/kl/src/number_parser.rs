use regex::Regex;
use std::path::Path;

pub fn get_number(path: &Path) -> Option<String> {
    let filename = path.file_name()?.to_str()?;

    // Check for FC2
    let lower_name = filename.to_lowercase();
    if lower_name.contains("fc2") {
        // e.g., FC2-PPV-123456 or FC2-123456
        let re = Regex::new(r"(?i)fc2(?:-ppv)?-(\d+)").unwrap();
        if let Some(caps) = re.captures(filename) {
            return Some(format!("FC2-{}", &caps[1]));
        }
    }

    let clean_name = clean_filename(filename);

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
