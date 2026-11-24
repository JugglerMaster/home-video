// State
let videos = [];
let playlists = [];
let currentVideo = null;
let currentPlaylist = null;
let filteredVideos = [];
let hls = null;

// Elements
const links = document.querySelectorAll(".tab-link");
const tabs = document.querySelectorAll(".tab");

// Home tab to highlight (and return to) when a detail view is active.
const DETAIL_HOME = { video: "library", playback: "playlist" };

function activate(name) {
    const homeForDetail = DETAIL_HOME[name] || name;
    links.forEach((l) => l.classList.toggle("active", l.dataset.tab === homeForDetail));
    tabs.forEach((t) => t.classList.toggle("active", t.id === "tab-" + name));
    if (name === "library") renderLibrary();
    if (name === "playlist") loadPlaylists();
    if (name === "admin") renderAdmin();
}

links.forEach((l) => {
    l.addEventListener("click", (e) => {
        e.preventDefault();
        activate(l.dataset.tab);
    });
});

// Back arrows (detail views return to their home tab).
document.getElementById("back-to-library").addEventListener("click", () => activate("library"));
document.getElementById("back-to-playlists").addEventListener("click", () => activate("playlist"));

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function esc(s) {
    return String(s ?? "").replace(/[&<>"']/g, (c) =>
        ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])
    );
}

function formatDuration(d) {
    if (!d) return "0:00";
    const m = Math.floor(d / 60);
    const s = Math.floor(d % 60);
    return m + ":" + (s < 10 ? "0" : "") + s;
}

function formatBytes(n) {
    if (!n) return "0 B";
    const u = ["B", "KB", "MB", "GB"];
    let i = 0;
    while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
    return n.toFixed(1) + " " + u[i];
}

async function fetchJson(url) {
    const r = await fetch(url);
    if (!r.ok) throw new Error("HTTP " + r.status);
    return r.json();
}

// ---------------------------------------------------------------------------
// Player helpers (shared by the single-video and playlist views)
// ---------------------------------------------------------------------------

function renderVideoMeta(metaEl, v) {
    let html =
        "<p>Duration: " + formatDuration(v.duration) + "</p>" +
        "<p>Resolution: " + esc(v.resolution) + "</p>" +
        "<p>Codec: " + esc(v.codec) + "</p>";
    if (v.size) html += "<p>Size: " + formatBytes(v.size) + "</p>";
    if (v.bitrate) html += "<p>Bitrate: " + (v.bitrate / 1000).toFixed(0) + " kbps</p>";
    metaEl.innerHTML = html;
}

function loadPlayer(playerEl, v, metaEl) {
    if (!v.m3u8) {
        playerEl.style.display = "none";
        metaEl.innerHTML +=
            "<p style='color:#ff5d5d;'>Not playable (conversion failed or unsupported codec).</p>";
        return;
    }
    if (hls) { hls.destroy(); hls = null; }
    playerEl.style.display = "block";
    if (typeof Hls !== "undefined" && Hls.isSupported()) {
        hls = new Hls({ hlsPlayOnMuted: true });
        hls.loadSource(v.m3u8);
        hls.attachMedia(playerEl);
    } else if (playerEl.canPlayType("application/vnd.apple.mpegurl")) {
        playerEl.src = v.m3u8;
    } else {
        metaEl.innerHTML +=
            "<p style='color:#ff5d5d;'>HLS not supported in this browser.</p>";
    }
    playerEl.load();
}

// Scale the player to fit its container while preserving aspect ratio.
function fitVideo(player) {
    if (!player || player.style.display === "none") return;
    const wrap = player.parentElement;
    const containerWidth = wrap.clientWidth || window.innerWidth;
    const naturalW = player.videoWidth || 0;
    const naturalH = player.videoHeight || 0;
    if (naturalW > 0 && naturalH > 0) {
        const aspect = naturalW / naturalH;
        const availableHeight = window.innerHeight * 0.78;
        const maxW = Math.min(containerWidth, availableHeight * aspect);
        if (maxW < containerWidth) {
            player.style.width = maxW + "px";
            player.style.marginLeft = "auto";
            player.style.marginRight = "auto";
        } else {
            player.style.width = "100%";
            player.style.marginLeft = "";
            player.style.marginRight = "";
        }
    } else {
        player.style.width = "100%";
    }
}

// ---------------------------------------------------------------------------
// Library
// ---------------------------------------------------------------------------

function renderLibraryCards() {
    const el = document.getElementById("library");
    const cards = filteredVideos.map(function (v) {
        const badge = v.m3u8 ? "" : "<div class='card-badge'>Not playable</div>";
        return "<div class='card' data-id='" + v.id + "'>" +
            "<img src='" + (v.thumbnail || "/static/thumbnail/default.jpg") + "' alt='' loading='lazy'>" +
            badge +
            "<div class='card-title'>" + esc(v.title) + "</div>" +
            "<div class='card-sub'>" + formatDuration(v.duration) + " \u00b7 " + esc(v.codec) + "</div>" +
        "</div>";
    }).join("");
    el.innerHTML = cards;
    el.querySelectorAll(".card").forEach(function (card) {
        card.addEventListener("click", function () {
            loadVideo(parseInt(card.dataset.id));
        });
    });
}

function applyFilters() {
    const q = document.getElementById("search-input").value.toLowerCase();
    const codec = document.getElementById("codec-filter").value.toLowerCase();
    filteredVideos = videos.filter((v) =>
        (v.title.toLowerCase().includes(q) ||
         v.description.toLowerCase().includes(q) ||
         v.tags.toLowerCase().includes(q)) &&
        (codec === "" || v.codec.toLowerCase().includes(codec))
    );
    renderLibraryCards();
}

function renderLibrary() {
    fetchJson("/api/videos").then((videos) => {
        videos = videos;
        applyFilters();
    });
}

// ---------------------------------------------------------------------------
// Single-video view
// ---------------------------------------------------------------------------

function loadVideo(id) {
    const v = videos.find((x) => x.id === id);
    if (!v) return;
    currentVideo = v;
    document.getElementById("video-title").textContent = v.title;
    document.getElementById("video-placeholder").style.display = "none";
    const player = document.getElementById("video-player");
    const meta = document.getElementById("video-meta");
    const edit = document.getElementById("video-edit");
    meta.style.display = "block";
    edit.style.display = "block";
    renderVideoMeta(meta, v);
    document.getElementById("video-desc").value = v.description || "";
    document.getElementById("video-tags").value = v.tags || "";
    document.getElementById("save-video-btn").onclick = function () {
        fetch("/api/videos/" + v.id, {
            method: "PUT",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({
                description: document.getElementById("video-desc").value,
                tags: document.getElementById("video-tags").value,
            })
        }).then(() => {
            activate("library");
        });
    };
    loadPlayer(player, v, meta);
    fitVideo(player);
    activate("video");
}

// ---------------------------------------------------------------------------
// Playlist playback view
// ---------------------------------------------------------------------------

function loadPlaylistPlayback(id) {
    activate("playback");
    const placeholder = document.getElementById("playback-placeholder");
    placeholder.style.display = "block";
    fetchJson("/api/playlists/" + id).then(function (p) {
        currentPlaylist = p;
        document.getElementById("playback-title").textContent = p.name;
        const listEl = document.getElementById("playback-list");
        const placeholder = document.getElementById("playback-placeholder");
        const wrap = document.getElementById("playback-wrap");
        const items = p.items || [];
        if (items.length === 0) {
            placeholder.style.display = "block";
            wrap.style.display = "none";
            listEl.innerHTML = "";
            return;
        }
        placeholder.style.display = "none";
        wrap.style.display = "block";
        listEl.innerHTML = items.map(function (i) {
            return "<div class='playback-item' data-id='" + i.video_id + "'>" +
                "<span class='playback-item-title'>" + esc(i.title) + "</span>" +
                "<span class='sub'>" + formatDuration(i.duration) + "</span>" +
            "</div>";
        }).join("");
        listEl.querySelectorAll(".playback-item").forEach(function (item) {
            item.addEventListener("click", function () {
                playPlaybackItem(item);
            });
        });
        playPlaybackItem(listEl.querySelector(".playback-item"));
    });
}

function playPlaybackItem(itemEl) {
    const id = parseInt(itemEl.dataset.id);
    const item = currentPlaylist && currentPlaylist.items
        ? currentPlaylist.items.find(function (x) { return x.video_id === id; })
        : null;
    if (!item) return;
    const listEl = document.getElementById("playback-list");
    listEl.querySelectorAll(".playback-item").forEach(function (el) { el.classList.remove("active"); });
    itemEl.classList.add("active");
    const player = document.getElementById("playback-player");
    const meta = document.getElementById("playback-meta");
    const wrap = document.getElementById("playback-wrap");
    wrap.style.display = "block";
    meta.style.display = "block";
    renderVideoMeta(meta, item);
    loadPlayer(player, item, meta);
    fitVideo(player);
}

// Add/remove controls, shown from the playback view.
function renderPlaylistManage(containerEl, playlistId) {
    const list = containerEl.querySelector(".manage-list");
    const addUrl = containerEl.querySelector(".manage-add-url");
    const addBtn = containerEl.querySelector(".manage-add-btn");
    fetchJson("/api/playlists/" + playlistId).then(function (p) {
        list.innerHTML = (p.items || []).map(function (i) {
            return "<div class='playlist-detail-item' data-id='" + i.video_id + "'>" +
                "<span class='playlist-detail-title'>" + esc(i.title) + "</span>" +
                "<button class='small-btn' data-remove='" + i.video_id + "'>Remove</button>" +
            "</div>";
        }).join("");
        list.querySelectorAll("[data-remove]").forEach(function (btn) {
            btn.addEventListener("click", function () {
                const vid = parseInt(btn.dataset.remove);
                fetch("/api/playlists/" + playlistId + "/videos/" + vid, { method: "DELETE" }).then(function () {
                    renderPlaylistManage(containerEl, playlistId);
                });
            });
        });
        addBtn.onclick = function () {
            const url = addUrl.value.trim();
            if (!url) return;
            fetch("/api/playlists/" + playlistId + "/videos", {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({ url: url })
            }).then(function () {
                renderPlaylistManage(containerEl, playlistId);
            });
        };
    });
}

document.getElementById("manage-playlist").addEventListener("click", function () {
    const manage = document.getElementById("playback-manage");
    const isHidden = manage.style.display === "none";
    manage.style.display = isHidden ? "block" : "none";
    if (isHidden && currentPlaylist) {
        renderPlaylistManage(manage, currentPlaylist.id);
    }
});

// ---------------------------------------------------------------------------
// Playlists (list)
// ---------------------------------------------------------------------------

function loadPlaylists() {
    fetchJson("/api/playlists").then((list) => {
        playlists = list;
        const el = document.getElementById("playlist-list");
        el.innerHTML = playlists.map((p) =>
            "<li class='playlist-item' data-id='" + p.id + "'>" +
            "<span class='playlist-name'>" + esc(p.name) + "</span>" +
            "<span class='playlist-count'>" + p.video_count + " video(s)</span>" +
            "</li>"
        ).join("");
        el.querySelectorAll(".playlist-item").forEach(function (item) {
            item.addEventListener("click", function () {
                loadPlaylistPlayback(parseInt(item.dataset.id));
            });
        });
    });
}

// ---------------------------------------------------------------------------
// Admin (login + dashboard)
// ---------------------------------------------------------------------------

function renderAdmin() {
    fetchJson("/api/session").then(function (j) {
        const login = document.getElementById("admin-login");
        const dashboard = document.getElementById("admin-dashboard");
        if (j.authenticated) {
            login.style.display = "none";
            dashboard.style.display = "block";
            renderAdminDashboard();
        } else {
            login.style.display = "block";
            dashboard.style.display = "none";
        }
    });
}

function renderAdminDashboard() {
    fetchJson("/api/videos").then(function (videos) {
        const el = document.getElementById("admin-videos");
        el.innerHTML = videos.map(function (v) {
            return "<div class='admin-video'>" +
                "<span class='admin-video-title'>" + esc(v.title) + "</span>" +
                "<button class='small-btn' data-del='" + v.id + "'>Delete</button>" +
            "</div>";
        }).join("");
        el.querySelectorAll("[data-del]").forEach(function (btn) {
            btn.addEventListener("click", function () {
                const id = parseInt(btn.dataset.del);
                fetch("/api/videos/" + id, { method: "DELETE" }).then(function () { renderAdmin(); });
            });
        });
    });
    fetchJson("/api/settings").then(function (s) {
        document.getElementById("watch-dirs").value = s.watch_dirs.join(", ");
        document.getElementById("anonymous-view").checked = s.anonymous_view;
    });
    fetch("/api/version").then(function (r) {
        r.json().then(function (j) {
            document.getElementById("version-banner").textContent =
                j.version + " \u2014 " + (j.outdated ? "Update available: " + j.latest : "up to date");
        });
    });
}

document.getElementById("admin-login-btn").addEventListener("click", function () {
    const password = document.getElementById("admin-password").value;
    fetch("/admin/login", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ password: password })
    }).then(function (r) {
        if (r.status === 401) {
            document.getElementById("admin-login-error").textContent = "Wrong password";
        } else {
            renderAdmin();
        }
    });
});

// ---------------------------------------------------------------------------
// Fullscreen
// ---------------------------------------------------------------------------

document.querySelectorAll("[data-fullscreen]").forEach(function (btn) {
    btn.addEventListener("click", function () {
        const target = document.getElementById(btn.dataset.fullscreen);
        if (!target) return;
        if (document.fullscreenElement === target) {
            document.exitFullscreen();
        } else {
            target.requestFullscreen().catch(function () {});
        }
    });
});

document.addEventListener("fullscreenchange", function () {
    const isFullscreen = document.fullscreenElement !== null;
    document.body.classList.toggle("fullscreen", isFullscreen);
    document.querySelectorAll("[data-fullscreen]").forEach(function (btn) {
        const target = document.getElementById(btn.dataset.fullscreen);
        if (target) {
            btn.textContent = document.fullscreenElement === target ? "Exit fullscreen" : "Fullscreen";
        }
    });
});

// ---------------------------------------------------------------------------
// Init
// ---------------------------------------------------------------------------

function init() {
    document.getElementById("search-input").addEventListener("input", applyFilters);
    document.getElementById("codec-filter").addEventListener("input", applyFilters);
    document.getElementById("new-playlist-btn").addEventListener("click", () => {
        const name = document.getElementById("new-playlist-name").value.trim();
        fetch("/api/playlists", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ name })
        }).then(() => {
            document.getElementById("new-playlist-name").value = "";
            loadPlaylists();
        });
    });
    document.getElementById("import-btn").addEventListener("click", () => {
        const url = document.getElementById("youtube-url").value.trim();
        if (!url) return;
        fetch("/api/import", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ url })
        }).then(() => {
            document.getElementById("youtube-url").value = "";
            renderLibrary();
        });
    });
    document.getElementById("save-watch-dirs").addEventListener("click", () => {
        const dirs = document.getElementById("watch-dirs").value.split(",").map(s => s.trim()).filter(Boolean);
        fetch("/api/settings", {
            method: "PUT",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ watch_dirs: dirs })
        }).then(() => {
            document.getElementById("watch-dirs-error").textContent = "";
        });
    });
    document.getElementById("save-anonymous").addEventListener("click", () => {
        fetch("/api/settings", {
            method: "PUT",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ anonymous_view: document.getElementById("anonymous-view").checked })
        }).then(() => {
            document.getElementById("anonymous-error").textContent = "";
        });
    });

    // Fit players when their metadata loads or the window resizes.
    const videoPlayer = document.getElementById("video-player");
    const playbackPlayer = document.getElementById("playback-player");
    videoPlayer.addEventListener("loadedmetadata", function () { fitVideo(this); });
    playbackPlayer.addEventListener("loadedmetadata", function () { fitVideo(this); });
    window.addEventListener("resize", function () {
        fitVideo(videoPlayer);
        fitVideo(playbackPlayer);
    });

    renderLibrary();
    loadPlaylists();
    setInterval(renderLibrary, 60000);
}

init();
