// Lightweight Node test for static/logic.js — extracted pure library logic.
// Run: node tests/logic.test.js
const assert = require("node:assert");
const logic = require("../static/logic.js");

// restoreSavedTab: valid values pass through, invalid/missing fall back.
assert.strictEqual(logic.restoreSavedTab("library"), "library");
assert.strictEqual(logic.restoreSavedTab("playlist"), "playlist");
assert.strictEqual(logic.restoreSavedTab("admin"), "admin");
assert.strictEqual(logic.restoreSavedTab("video"), "library");
assert.strictEqual(logic.restoreSavedTab("playlist"), "playlist"); // repeat
assert.strictEqual(logic.restoreSavedTab(""), "library");
assert.strictEqual(logic.restoreSavedTab(undefined), "library");
assert.strictEqual(logic.restoreSavedTab(null), "library");

// homeForTab: DETAIL_HOME mapping.
assert.strictEqual(logic.homeForTab("video"), "library");
assert.strictEqual(logic.homeForTab("playback"), "playlist");
assert.strictEqual(logic.homeForTab("library"), "library"); // passthrough
assert.strictEqual(logic.homeForTab("admin"), "admin");
assert.strictEqual(logic.homeForTab("unknown"), "unknown"); // fail open

// debounce: trailing call only after ms, repeated calls in window are ignored.
(async () => {
    let calls = 0;
    const debounced = logic.debounce(() => { calls++; }, 50);
    debounced();
    debounced();
    debounced();
    await new Promise((r) => setTimeout(r, 150));
    assert.strictEqual(calls, 1);

    let calls2 = 0;
    const debounced2 = logic.debounce(() => { calls2++; }, 50);
    debounced2();
    await new Promise((r) => setTimeout(r, 150));
    assert.strictEqual(calls2, 1);

    console.log("logic.test.js: all assertions passed");
})().catch((e) => { console.error(e); process.exit(1); });
