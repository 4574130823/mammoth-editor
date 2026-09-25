//! Classifies email domains into providers (Gmail, Yahoo, Outlook, …) for the
//! email breakdown. Unknown domains are treated as company / custom domains.

const PROVIDERS: &[(&str, &[&str])] = &[
    ("Gmail", &["gmail.com", "googlemail.com"]),
    (
        "Outlook / Hotmail",
        &[
            "outlook.com",
            "hotmail.com",
            "live.com",
            "msn.com",
            "passport.com",
            "windowslive.com",
        ],
    ),
    ("Yahoo", &["yahoo.com", "ymail.com", "rocketmail.com"]),
    ("iCloud", &["icloud.com", "me.com", "mac.com"]),
    ("AOL", &["aol.com", "aim.com"]),
    (
        "Proton",
        &["proton.me", "protonmail.com", "protonmail.ch", "pm.me"],
    ),
    ("Zoho", &["zoho.com", "zohomail.com"]),
    ("Fastmail", &["fastmail.com", "fastmail.fm"]),
    (
        "Tuta",
        &[
            "tuta.com",
            "tuta.io",
            "tutanota.com",
            "tutanota.de",
            "tutamail.com",
        ],
    ),
    ("GMX", &["gmx.com", "gmx.net", "gmx.de", "gmx.at", "gmx.ch"]),
    ("Web.de", &["web.de"]),
    ("Yandex", &["yandex.ru", "yandex.com", "ya.ru"]),
    ("Mail.ru", &["mail.ru", "inbox.ru", "list.ru", "bk.ru"]),
    ("QQ", &["qq.com", "foxmail.com"]),
    ("NetEase", &["163.com", "126.com", "yeah.net"]),
    ("Naver", &["naver.com"]),
    ("Daum", &["daum.net", "hanmail.net"]),
    ("Comcast / Xfinity", &["comcast.net"]),
    ("AT&T", &["att.net", "sbcglobal.net", "bellsouth.net"]),
    ("Verizon", &["verizon.net"]),
    ("Orange", &["orange.fr", "wanadoo.fr"]),
    ("Free", &["free.fr"]),
    ("Libero", &["libero.it"]),
    ("Rediffmail", &["rediffmail.com"]),
    (
        "Disposable",
        &[
            "mailinator.com",
            "guerrillamail.com",
            "sharklasers.com",
            "10minutemail.com",
            "temp-mail.org",
            "yopmail.com",
            "trashmail.com",
            "getnada.com",
            "dispostable.com",
            "maildrop.cc",
            "throwawaymail.com",
        ],
    ),
];

/// Providers that also use country domains, e.g. yahoo.co.uk or hotmail.fr.
const FAMILIES: &[(&str, &str)] = &[
    ("gmail", "Gmail"),
    ("yahoo", "Yahoo"),
    ("hotmail", "Outlook / Hotmail"),
    ("outlook", "Outlook / Hotmail"),
    ("live", "Outlook / Hotmail"),
    ("aol", "AOL"),
    ("gmx", "GMX"),
    ("yandex", "Yandex"),
];

pub const OTHER: &str = "Company / other";

pub fn provider(domain: &str) -> &'static str {
    let d = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    if let Some((name, _)) = PROVIDERS
        .iter()
        .find(|(_, list)| list.contains(&d.as_str()))
    {
        return name;
    }
    let labels: Vec<&str> = d.split('.').collect();
    // "yahoo.co.uk", "hotmail.fr", "yahoo.com.br": a known first label followed
    // only by short, country-style labels.
    if labels.len() <= 3
        && labels[1..].iter().all(|l| l.len() <= 3)
        && let Some((_, name)) = FAMILIES.iter().find(|(f, _)| *f == labels[0])
    {
        return name;
    }
    let has = |part: &str| labels.iter().rev().take(3).any(|l| *l == part);
    if d.ends_with(".edu") || ((has("edu") || has("ac")) && labels.len() >= 3) {
        return "Education";
    }
    if d.ends_with(".gov") || d.ends_with(".mil") || (has("gov") && labels.len() >= 3) {
        return "Government";
    }
    OTHER
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_providers() {
        assert_eq!(provider("gmail.com"), "Gmail");
        assert_eq!(provider("GoogleMail.com"), "Gmail");
        assert_eq!(provider("yahoo.co.uk"), "Yahoo");
        assert_eq!(provider("yahoo.com.br"), "Yahoo");
        assert_eq!(provider("hotmail.fr"), "Outlook / Hotmail");
        assert_eq!(provider("live.com"), "Outlook / Hotmail");
        assert_eq!(provider("me.com"), "iCloud");
        assert_eq!(provider("mit.edu"), "Education");
        assert_eq!(provider("ox.ac.uk"), "Education");
        assert_eq!(provider("irs.gov"), "Government");
        assert_eq!(provider("mailinator.com"), "Disposable");
        assert_eq!(provider("live.example.com"), OTHER);
        assert_eq!(provider("acme-corp.com"), OTHER);
    }
}
