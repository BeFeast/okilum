//! Production execution stays explicitly unavailable until a supported adapter
//! exists. Tracking an ordinary Manager request never establishes a build.
pub(super) fn availability_notice() -> &'static str {
    okilum_t3::express_execution::BLOCKER
}
