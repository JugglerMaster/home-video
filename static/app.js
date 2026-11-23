// home-video UI skeleton (S0).
// Tab switching + health-check polling. Data wiring lands in S2/S4/S5/S6.

(function () {
  "use strict";

  // --- Tabs ---
  var links = document.querySelectorAll(".tab-link");
  var tabs = document.querySelectorAll(".tab");

  function activate(name) {
    links.forEach(function (l) {
      l.classList.toggle("active", l.dataset.tab === name);
    });
    tabs.forEach(function (t) {
      t.classList.toggle("active", t.id === "tab-" + name);
    });
  }

  links.forEach(function (l) {
    l.addEventListener("click", function (e) {
      e.preventDefault();
      activate(l.dataset.tab);
    });
  });

  // --- Health check ---
  var healthEl = document.getElementById("health");

  function pollHealth() {
    fetch("/health")
      .then(function (r) {
        return r.ok ? r : Promise.reject(r);
      })
      .then(function () {
        healthEl.textContent = "ok";
        healthEl.className = "ok";
      })
      .catch(function () {
        healthEl.textContent = "unreachable";
        healthEl.className = "err";
      });
  }

  pollHealth();
  setInterval(pollHealth, 5000);
})();
