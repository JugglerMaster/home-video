// home-video UI. Data wiring lands across S2/S4/S5/S6/S7.
(function () {
  "use strict";

  // --- State ---
  var videos = [];
  var currentVideo = null;
  var hls = null;

  // --- Tabs ---
  var links = document.querySelectorAll(".tab-link");
  var tabs = document.querySelectorAll(".tab");

  function activate(name) {
    links.forEach(function (l) { l.classList.toggle("active", l.dataset.tab === name); });
    tabs.forEach(function (t) { t.classList.toggle("active", t.id === "tab-" + name); });
    if (name === "library") renderLibrary();
    if (name === "playlist") loadPlaylists();
    if (name === "admin") renderAdmin();
  }
  links.forEach(function (l) {
    l.addEventListener("click", function (e) { e.preventDefault(); activate(l.dataset.tab); });
  });

  // --- Helpers ---
  function esc(s) {
    return (s == null ? "" : String(s)).replace(/[&<>"']/g, function (c) {
      return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c];
    });
  }
  function formatDuration(secs) {
    if (!secs || secs <= 0) return "0:00";
    var m = Math.floor(secs / 60);
    var s = Math.floor(secs % 60);
    return m + ":" + (s < 10 ? "0" : "") + s;
  }
  function formatBytes(n) {
    if (!n) return "0 B";
    var units = ["B", "KB", "MB", "GB"];
    var i = 0;
    while (n >= 1024 && i < units.length - 1) { n /= 1024; i++; }
    return n.toFixed(1) + " " + units[i];
  }

  // --- Health check ---
  var healthEl = document.getElementById("health");
  function pollHealth() {
    fetch("/health")
      .then(function (r) { return r.ok ? r : Promise.reject(r); })
      .then(function () { healthEl.textContent = "ok"; healthEl.className = "ok"; })
      .catch(function () { healthEl.textContent = "unreachable"; healthEl.className = "err"; });
  }
  pollHealth();
  setInterval(pollHealth, 5000);

  // --- Library (S2) ---
  var gridEl = document.getElementById("video-grid");
  var placeholderEl = document.getElementById("grid-placeholder");
  var searchEl = document.getElementById("search-input");
  var filterResEl = document.getElementById("filter-resolution");
  var filterCodecEl = document.getElementById("filter-codec");

  function fetchVideos() {
    return fetch("/api/videos")
      .then(function (r) { if (!r.ok) throw new Error("fetch failed " + r.status); return r.json(); })
      .then(function (v) { return Array.isArray(v) ? v : []; });
  }

  function renderLibrary() {
    fetchVideos().then(function (list) {
      videos = list;
      populateFilters();
      applyFilters();
    }).catch(function () {
      gridEl.innerHTML = "";
      placeholderEl.style.display = "";
      placeholderEl.textContent = "No videos yet (drop one into a watch dir).";
    });
  }

  function populateFilters() {
    var res = new Set(videos.map(function (v) { return v.resolution; }));
    var codec = new Set(videos.map(function (v) { return v.codec; }));
    filterResEl.innerHTML = '<option value="">All resolutions</option>' +
      Array.from(res).sort().map(function (r) { return '<option value="' + esc(r) + '">' + esc(r) + '</option>'; }).join("");
    filterCodecEl.innerHTML = '<option value="">All codecs</option>' +
      Array.from(codec).sort().map(function (c) { return '<option value="' + esc(c) + '">' + esc(c) + '</option>'; }).join("");
  }

  function applyFilters() {
    var q = searchEl.value.toLowerCase();
    var res = filterResEl.value;
    var codec = filterCodecEl.value;
    var filtered = videos.filter(function (v) {
      if (res && v.resolution !== res) return false;
      if (codec && v.codec !== codec) return false;
      if (q) {
        var hay = (v.title + " " + (v.tags || "") + " " + (v.description || "")).toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    });
    gridEl.innerHTML = filtered.map(function (v) {
      return '<div class="card" data-id="' + v.id + '">' +
        '<img src="' + v.thumbnail + '" loading="lazy" alt="' + esc(v.title) + '">' +
        '<div class="card-meta">' +
          '<div class="card-title">' + esc(v.title) + '</div>' +
          '<div class="card-sub">' + formatDuration(v.duration) + ' · ' + esc(v.resolution) + ' · ' + esc(v.codec) + '</div>' +
        '</div>' +
      '</div>';
    }).join("");
    placeholderEl.style.display = filtered.length ? "none" : "";
    placeholderEl.textContent = filtered.length ? "" : "No videos match.";
    gridEl.querySelectorAll(".card").forEach(function (card) {
      card.addEventListener("click", function () { loadVideo(parseInt(card.dataset.id)); });
    });
  }

  function loadVideo(id) {
    var v = videos.find(function (x) { return x.id === id; });
    if (!v) return;
    currentVideo = v;
    document.getElementById("video-title").textContent = v.title;
    document.getElementById("video-placeholder").style.display = "none";
    var player = document.getElementById("video-player");
    player.style.display = "block";
    var meta = document.getElementById("video-meta");
    meta.style.display = "block";
    meta.innerHTML =
      '<p>Duration: ' + formatDuration(v.duration) + '</p>' +
      '<p>Resolution: ' + esc(v.resolution) + '</p>' +
      '<p>Codec: ' + esc(v.codec) + '</p>' +
      '<p>Size: ' + formatBytes(v.size) + '</p>' +
      '<p>Bitrate: ' + (v.bitrate / 1000).toFixed(0) + ' kbps</p>';
    var edit = document.getElementById("video-edit");
    edit.style.display = "block";
    document.getElementById("edit-description").value = v.description || "";
    document.getElementById("edit-tags").value = v.tags || "";
    // Load HLS.
    if (hls) { hls.destroy(); hls = null; }
    if (typeof Hls !== "undefined" && Hls.isSupported()) {
      hls = new Hls({ hlsPlayOnMuted: true });
      hls.loadSource(v.m3u8);
      hls.attachMedia(player);
    } else if (player.canPlayType("application/vnd.apple.mpegurl")) {
      player.src = v.m3u8;
    } else {
      meta.innerHTML += '<p style="color:#ff5d5d;">HLS not supported in this browser.</p>';
    }
  }

  // Save video tags/description (S4).
  document.getElementById("save-video").addEventListener("click", function () {
    if (!currentVideo) return;
    var desc = document.getElementById("edit-description").value;
    var tags = document.getElementById("edit-tags").value;
    fetch("/api/videos/" + currentVideo.id, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ description: desc, tags: tags })
    }).then(function (r) {
      if (r.ok) {
        var msg = document.createElement("p");
        msg.style.color = "var(--accent)";
        msg.textContent = "Saved.";
        meta.appendChild(msg);
      }
    });
  });

  searchEl.addEventListener("input", applyFilters);
  filterResEl.addEventListener("change", applyFilters);
  filterCodecEl.addEventListener("change", applyFilters);

  // Auto-refresh (60s).
  setInterval(renderLibrary, 60000);
  renderLibrary();

  // --- Playlist (S5) ---
  var playlistListEl = document.getElementById("playlist-list");
  var playlistDetailEl = document.getElementById("playlist-detail");

  function loadPlaylists() {
    fetch("/api/playlists").then(function (r) {
      if (!r.ok) { playlistListEl.innerHTML = ""; playlistDetailEl.innerHTML = ""; return; }
    }).then(function (list) {
      playlistListEl.innerHTML = list.map(function (p) {
        return '<div class="list-item" data-id="' + p.id + '">' +
          '<span>' + esc(p.name) + '</span>' +
          '<span class="sub">' + (p.source === "youtube" ? "YouTube" : "manual") + ' · ' + (p.count || 0) + ' videos</span>' +
        '</div>';
      }).join("");
      playlistListEl.querySelectorAll(".list-item").forEach(function (item) {
        item.addEventListener("click", function () { loadPlaylistDetail(parseInt(item.dataset.id)); });
      });
    }).catch(function () { playlistListEl.innerHTML = ""; playlistDetailEl.innerHTML = ""; });
  }

  function loadPlaylistDetail(id) {
    fetch("/api/playlists/" + id).then(function (r) {
      if (!r.ok) { playlistDetailEl.innerHTML = ""; return; }
      return r.json();
    }).then(function (p) {
      playlistDetailEl.innerHTML =
        '<h3>' + esc(p.name) + '</h3>' +
        '<div class="list">' +
        (p.items || []).map(function (i) {
          return '<div class="item"><span>' + esc(i.title) + '</span>' +
            '<button data-id="' + i.video_id + '" class="remove-item">Remove</button></div>';
        }).join("") +
        '</div>' +
        '<div style="margin-top:0.6rem;"><input type="text" id="add-playlist-title" placeholder="Add video title" style="width:70%; background:var(--panel); color:var(--fg); border:1px solid #2a2a2a; border-radius:6px; padding:0.3rem 0.5rem;">' +
        '<button id="add-playlist-btn">Add</button></div>';
      playlistDetailEl.querySelectorAll(".remove-item").forEach(function (btn) {
        btn.addEventListener("click", function () {
          fetch("/api/playlists/" + id + "/items", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ video_id: parseInt(btn.dataset.id), action: "remove" })
          }).then(loadPlaylistDetail).catch(function () { loadPlaylistDetail(id); });
        });
      });
      var addBtn = document.getElementById("add-playlist-btn");
      if (addBtn) {
        addBtn.addEventListener("click", function () {
          // Placeholder: full add-by-search lands with S5 backend.
          loadPlaylistDetail(id);
        });
      }
    }).catch(function () { playlistDetailEl.innerHTML = ""; });
  }

  // --- Admin (S6/S7) ---
  var adminVideosEl = document.getElementById("admin-videos");
  var versionBanner = document.getElementById("version-banner");

  function renderAdmin() {
    fetch("/api/videos").then(function (r) {
      if (!r.ok) { adminVideosEl.innerHTML = ""; return; }
      return r.json();
    }).then(function (list) {
      adminVideosEl.innerHTML = list.map(function (v) {
        return '<div class="list-item">' +
          '<span>' + esc(v.title) + ' <span class="sub">(id ' + v.id + ')</span></span>' +
          '<button class="delete-video" data-id="' + v.id + '">Delete</button></div>';
      }).join("");
      adminVideosEl.querySelectorAll(".delete-video").forEach(function (btn) {
        btn.addEventListener("click", function () {
          if (!confirm("Delete this video?")) return;
          fetch("/api/videos/" + btn.dataset.id, { method: "DELETE" }).then(function () {
            adminVideosEl.innerHTML = "";
            renderLibrary();
          }).catch(function () { alert("Delete failed"); });
        });
      });
    }).catch(function () { adminVideosEl.innerHTML = ""; });
    checkVersion();
  }

  function checkVersion() {
    fetch("/api/version").then(function (r) {
      if (!r.ok) return;
      return r.json();
    }).then(function (v) {
      if (v.update_available) {
        versionBanner.style.display = "block";
        versionBanner.innerHTML =
          'Update available: current ' + esc(v.current_version) +
          ' → latest ' + esc(v.latest_version) +
          ' <span class="link" onclick="location.href=\'' + esc(v.repo_url) + '\'">repo</span>';
      } else {
        versionBanner.style.display = "none";
      }
    }).catch(function () { versionBanner.style.display = "none"; });
  }
  setInterval(checkVersion, 60000);

  // YouTube import (S3).
  var ytBtn = document.getElementById("yt-import-btn");
  var ytUrl = document.getElementById("yt-url");
  if (ytBtn) {
    ytBtn.addEventListener("click", function () {
      var url = ytUrl.value.trim();
      if (!url) return;
      fetch("/api/import", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ url: url })
      }).then(function (r) {
        if (r.ok) {
          ytUrl.value = "";
          renderLibrary();
        } else {
          r.json().then(function (e) { alert("Import failed: " + (e.error || e.message)); });
        }
      }).catch(function () { alert("Import request failed"); });
    });
  }
})();
