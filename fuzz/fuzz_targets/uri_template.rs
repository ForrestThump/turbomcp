#![no_main]
//! Resource URI-template matching compiles an attacker-influenced template
//! (from a `#[resource("...")]` — trusted) against an attacker-controlled URI
//! (from `resources/read` — untrusted). Neither the template compile nor the
//! match may panic or hang; a malformed template yields `None`.

use libfuzzer_sys::fuzz_target;
use turbomcp_server::__macro_support::match_uri_template;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };
    // Split the input into a "template" and a "uri" on the first NUL so one
    // corpus entry exercises both arguments; fall back to matching the whole
    // string against itself.
    let (template, uri) = s.split_once('\0').unwrap_or((s, s));
    let _ = match_uri_template(template, uri);
});
