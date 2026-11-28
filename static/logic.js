// Pure helpers for the library tab logic, extracted from app.js so they can
// be unit-tested in Node without a DOM. Exposed as window.AppLogic in the
// browser and module.exports in Node.

const DETAIL_HOME = { video: "library", playback: "playlist" };
const VALID_TABS = ["library", "playlist", "admin"];

// Return the valid tab/section name for a saved value, falling back to "library".
function restoreSavedTab(saved) {
    return VALID_TABS.includes(saved) ? saved : "library";
}

// Home tab to highlight when a detail view is active.
function homeForTab(name) {
    return DETAIL_HOME[name] || name;
}

// Home tab persisted for a refresh.
function persistedHomeForTab(name) {
    return homeForTab(name);
}

// Simple trailing debounce.
function debounce(fn, ms) {
    let timer = null;
    return function () {
        if (timer) clearTimeout(timer);
        timer = setTimeout(fn, ms);
    };
}

if (typeof window !== "undefined") {
    window.AppLogic = { DETAIL_HOME, VALID_TABS, restoreSavedTab, homeForTab, persistedHomeForTab, debounce };
}

if (typeof module !== "undefined" && module.exports) {
    module.exports = { DETAIL_HOME, VALID_TABS, restoreSavedTab, homeForTab, persistedHomeForTab, debounce };
}
