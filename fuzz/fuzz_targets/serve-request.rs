#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    recast_radar_fuzz::serve_request(data);
});
