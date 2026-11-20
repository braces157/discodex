(function () {
  "use strict";

  var root = document.documentElement;
  var reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  if (/[?&]static(=|&|$)/.test(window.location.search)) root.classList.add("static");
  var motion = root.classList.contains("motion");

  var scrollProgress = document.getElementById("scrollProgress");
  var progressFrame = 0;
  function renderScrollProgress() {
    var scrollable = document.documentElement.scrollHeight - window.innerHeight;
    var progress = scrollable > 0 ? Math.min(1, Math.max(0, window.scrollY / scrollable)) : 0;
    scrollProgress.style.transform = "scaleX(" + progress.toFixed(4) + ")";
    progressFrame = 0;
  }
  if (!reducedMotion && !root.classList.contains("static")) {
    window.addEventListener("scroll", function () {
      if (!progressFrame) progressFrame = window.requestAnimationFrame(renderScrollProgress);
    }, { passive: true });
    window.addEventListener("resize", renderScrollProgress, { passive: true });
    renderScrollProgress();
  }

  var revealElements = document.querySelectorAll(".reveal");
  if ("IntersectionObserver" in window && !root.classList.contains("static") && !reducedMotion) {
    var revealObserver = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (entry.isIntersecting) {
          entry.target.classList.add("in");
          revealObserver.unobserve(entry.target);
        }
      });
    }, { threshold: 0.08, rootMargin: "0px 0px -5% 0px" });
    revealElements.forEach(function (element) { revealObserver.observe(element); });
  } else {
    revealElements.forEach(function (element) { element.classList.add("in"); });
  }

  var countValue = document.querySelector(".count-value");
  if (countValue && "IntersectionObserver" in window && !reducedMotion && !root.classList.contains("static")) {
    var countTarget = Number(countValue.dataset.count);
    var countObserver = new IntersectionObserver(function (entries) {
      if (!entries[0].isIntersecting) return;
      countObserver.disconnect();
      var countStart = 0;
      function tickCount(timestamp) {
        if (!countStart) countStart = timestamp;
        var progress = Math.min(1, (timestamp - countStart) / 1100);
        var eased = 1 - Math.pow(1 - progress, 3);
        countValue.textContent = String(Math.round(countTarget * eased));
        if (progress < 1) window.requestAnimationFrame(tickCount);
      }
      countValue.textContent = "0";
      window.requestAnimationFrame(tickCount);
    }, { threshold: 0.45 });
    countObserver.observe(countValue);
  }

  var menuToggle = document.getElementById("menuToggle");
  var mobileMenu = document.getElementById("mobileMenu");
  var menuOpen = false;
  function setMenu(open) {
    menuOpen = open;
    menuToggle.classList.toggle("open", open);
    mobileMenu.classList.toggle("open", open);
    menuToggle.setAttribute("aria-expanded", String(open));
    menuToggle.setAttribute("aria-label", open ? "Close menu" : "Open menu");
    mobileMenu.setAttribute("aria-hidden", String(!open));
    document.body.style.overflow = open ? "hidden" : "";
    if (open) mobileMenu.querySelector("a").focus();
    else if (document.activeElement && mobileMenu.contains(document.activeElement)) menuToggle.focus();
  }
  menuToggle.addEventListener("click", function () { setMenu(!menuOpen); });
  mobileMenu.querySelectorAll("a").forEach(function (link) {
    link.addEventListener("click", function () { setMenu(false); });
  });
  document.addEventListener("keydown", function (event) {
    if (event.key === "Escape" && menuOpen) {
      setMenu(false);
      menuToggle.focus();
    }
    if (event.key === "Tab" && menuOpen) {
      var focusable = [menuToggle].concat(Array.prototype.slice.call(mobileMenu.querySelectorAll("a")));
      var first = focusable[0];
      var last = focusable[focusable.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    }
  });
  window.addEventListener("resize", function () {
    if (window.innerWidth > 680 && menuOpen) setMenu(false);
  }, { passive: true });

  var navLinks = Array.prototype.slice.call(document.querySelectorAll(".nav-links a"));
  var observedSections = navLinks.map(function (link) {
    return document.querySelector(link.getAttribute("href"));
  }).filter(Boolean);
  if ("IntersectionObserver" in window) {
    var sectionObserver = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (entry.isIntersecting) {
          navLinks.forEach(function (link) {
            if (link.getAttribute("href") === "#" + entry.target.id) link.setAttribute("aria-current", "location");
            else link.removeAttribute("aria-current");
          });
        }
      });
    }, { rootMargin: "-15% 0px -65% 0px", threshold: 0 });
    observedSections.forEach(function (section) { sectionObserver.observe(section); });
  }

  var startedAt = Date.now() - ((42 * 60 + 18) * 1000);
  var elapsed = document.getElementById("elapsed");
  function twoDigits(number) { return String(number).padStart(2, "0"); }
  function updateClock() {
    var totalSeconds = Math.floor((Date.now() - startedAt) / 1000);
    elapsed.textContent = twoDigits(Math.floor(totalSeconds / 3600)) + ":" + twoDigits(Math.floor((totalSeconds % 3600) / 60)) + ":" + twoDigits(totalSeconds % 60);
  }
  updateClock();
  window.setInterval(updateClock, 1000);

  var sceneStage = document.getElementById("sceneStage");
  var finePointer = window.matchMedia("(hover: hover) and (pointer: fine)");
  if (sceneStage && finePointer.matches && !reducedMotion && !root.classList.contains("static")) {
    var sceneFrame = 0;
    var pointerX = 0;
    var pointerY = 0;
    var scrollTilt = 0;
    function renderScene() {
      sceneStage.style.setProperty("--tilt-x", (-pointerY * 5).toFixed(2) + "deg");
      sceneStage.style.setProperty("--tilt-y", (pointerX * 7).toFixed(2) + "deg");
      sceneStage.style.setProperty("--scroll-tilt", scrollTilt.toFixed(2) + "deg");
      sceneStage.style.setProperty("--scene-shift", (scrollTilt * -1.2).toFixed(2) + "px");
      sceneFrame = 0;
    }
    function queueScene() {
      if (!sceneFrame) sceneFrame = window.requestAnimationFrame(renderScene);
    }
    sceneStage.addEventListener("pointermove", function (event) {
      var bounds = sceneStage.getBoundingClientRect();
      pointerX = Math.max(-1, Math.min(1, ((event.clientX - bounds.left) / bounds.width - 0.5) * 2));
      pointerY = Math.max(-1, Math.min(1, ((event.clientY - bounds.top) / bounds.height - 0.5) * 2));
      sceneStage.style.setProperty("--light-x", ((pointerX + 1) * 30 + 20).toFixed(1) + "%");
      sceneStage.style.setProperty("--light-y", ((pointerY + 1) * 25 + 10).toFixed(1) + "%");
      queueScene();
    }, { passive: true });
    sceneStage.addEventListener("pointerleave", function () {
      pointerX = 0;
      pointerY = 0;
      sceneStage.style.setProperty("--light-x", "35%");
      sceneStage.style.setProperty("--light-y", "10%");
      queueScene();
    }, { passive: true });
    window.addEventListener("scroll", function () {
      var bounds = sceneStage.getBoundingClientRect();
      scrollTilt = Math.max(-3, Math.min(3, (bounds.top + bounds.height * 0.5 - window.innerHeight * 0.5) / window.innerHeight * -4));
      queueScene();
    }, { passive: true });
  }

  var depthCards = document.querySelectorAll(".depth-card");
  if (finePointer.matches && !reducedMotion && !root.classList.contains("static")) {
    depthCards.forEach(function (card) {
      var depthFrame = 0;
      var depthX = 0;
      var depthY = 0;
      function renderDepth() {
        card.style.setProperty("--depth-rx", (-depthY * 3.2).toFixed(2) + "deg");
        card.style.setProperty("--depth-ry", (depthX * 4.2).toFixed(2) + "deg");
        depthFrame = 0;
      }
      function queueDepth() {
        if (!depthFrame) depthFrame = window.requestAnimationFrame(renderDepth);
      }
      card.addEventListener("pointermove", function (event) {
        var bounds = card.getBoundingClientRect();
        depthX = Math.max(-1, Math.min(1, ((event.clientX - bounds.left) / bounds.width - 0.5) * 2));
        depthY = Math.max(-1, Math.min(1, ((event.clientY - bounds.top) / bounds.height - 0.5) * 2));
        card.style.setProperty("--glare-x", ((depthX + 1) * 50).toFixed(1) + "%");
        card.style.setProperty("--glare-y", ((depthY + 1) * 50).toFixed(1) + "%");
        card.style.setProperty("--depth-z", "7px");
        card.classList.add("is-tilting");
        queueDepth();
      }, { passive: true });
      card.addEventListener("pointerleave", function () {
        depthX = 0;
        depthY = 0;
        card.style.setProperty("--depth-z", "0px");
        card.classList.remove("is-tilting");
        queueDepth();
      }, { passive: true });
    });
  }

  var providerChoices = document.querySelectorAll(".provider-choice");
  var presenceCard = document.getElementById("presenceCard");
  var providerImage = document.getElementById("providerImage");
  var providerName = document.getElementById("providerName");
  var activityPhase = document.getElementById("activityPhase");
  var activityFile = document.getElementById("activityFile");
  var providerSwitcher = document.querySelector(".provider-switcher");
  var providerSwapTimer = 0;
  var typeTimer = 0;
  function typePhase(text) {
    window.clearInterval(typeTimer);
    if (!motion) {
      activityPhase.textContent = text;
      return;
    }
    var shown = 0;
    activityPhase.textContent = "";
    activityPhase.classList.add("typing");
    typeTimer = window.setInterval(function () {
      shown += 1;
      activityPhase.textContent = text.slice(0, shown);
      if (shown >= text.length) {
        window.clearInterval(typeTimer);
        window.setTimeout(function () { activityPhase.classList.remove("typing"); }, 900);
      }
    }, 34);
  }
  function selectProvider(choice) {
    providerChoices.forEach(function (item) {
      item.classList.toggle("selected", item === choice);
      item.setAttribute("aria-pressed", String(item === choice));
    });
    providerImage.src = "assets/logos/" + choice.dataset.provider + ".png";
    providerName.textContent = choice.dataset.name;
    if (choice.dataset.file) activityFile.textContent = choice.dataset.file;
    typePhase(choice.dataset.phase);
    if (motion) {
      window.clearTimeout(providerSwapTimer);
      presenceCard.classList.remove("switching");
      void presenceCard.offsetWidth;
      presenceCard.classList.add("switching");
      providerSwapTimer = window.setTimeout(function () { presenceCard.classList.remove("switching"); }, 600);
    }
  }

  // Cycles the preview until the visitor picks a provider themselves.
  var autoplayMs = 4800;
  var autoplayTimer = 0;
  var autoplayStopped = !motion;
  var heroVisible = true;
  function scheduleAutoplay() {
    window.clearTimeout(autoplayTimer);
    providerSwitcher.classList.remove("autoplaying");
    if (autoplayStopped || !heroVisible || document.hidden) return;
    void providerSwitcher.offsetWidth;
    providerSwitcher.classList.add("autoplaying");
    autoplayTimer = window.setTimeout(function () {
      var list = Array.prototype.slice.call(providerChoices);
      var current = list.findIndex(function (item) { return item.classList.contains("selected"); });
      selectProvider(list[(current + 1) % list.length]);
      scheduleAutoplay();
    }, autoplayMs);
  }
  function stopAutoplay() {
    autoplayStopped = true;
    window.clearTimeout(autoplayTimer);
    providerSwitcher.classList.remove("autoplaying");
  }
  providerSwitcher.style.setProperty("--autoplay-ms", autoplayMs + "ms");
  providerChoices.forEach(function (choice) {
    choice.addEventListener("click", function () {
      stopAutoplay();
      selectProvider(choice);
    });
  });
  providerSwitcher.addEventListener("focusin", stopAutoplay);
  if (motion && "IntersectionObserver" in window) {
    new IntersectionObserver(function (entries) {
      heroVisible = entries[0].isIntersecting;
      scheduleAutoplay();
    }, { threshold: 0.25 }).observe(document.getElementById("sceneStage"));
    document.addEventListener("visibilitychange", scheduleAutoplay);
  }

  var search = document.getElementById("providerSearch");
  var providers = Array.prototype.slice.call(document.querySelectorAll("#providerList li"));
  var filters = Array.prototype.slice.call(document.querySelectorAll(".filter"));
  var count = document.getElementById("providerCount");
  var empty = document.getElementById("providerEmpty");
  var activeFilter = "all";
  var filtersReady = false;
  var providerList = document.getElementById("providerList");
  function filterProviders() {
    var query = search.value.trim().toLocaleLowerCase();
    var visible = 0;
    providers.forEach(function (provider) {
      var matchesFilter = activeFilter === "all" || provider.dataset.type === activeFilter;
      var matchesQuery = !query || provider.dataset.name.toLocaleLowerCase().indexOf(query) !== -1;
      provider.hidden = !(matchesFilter && matchesQuery);
      if (!provider.hidden) visible += 1;
    });
    count.textContent = "Showing " + visible + " of " + providers.length + " provider targets";
    empty.hidden = visible !== 0;
    if (motion && filtersReady) {
      var order = 0;
      providers.forEach(function (provider) {
        provider.classList.remove("pop");
        if (!provider.hidden) provider.style.setProperty("--i", String(Math.min(order++, 16)));
      });
      void providerList.offsetWidth;
      providers.forEach(function (provider) { if (!provider.hidden) provider.classList.add("pop"); });
    }
  }
  filters.forEach(function (filter) {
    filter.addEventListener("click", function () {
      activeFilter = filter.dataset.filter;
      filters.forEach(function (item) {
        item.classList.toggle("active", item === filter);
        item.setAttribute("aria-pressed", String(item === filter));
      });
      filterProviders();
    });
  });
  search.addEventListener("input", filterProviders);
  document.addEventListener("keydown", function (event) {
    if (event.key === "/" && !event.ctrlKey && !event.metaKey && !event.altKey && !/^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement.tagName)) {
      event.preventDefault();
      search.focus();
    }
  });
  document.getElementById("clearSearch").addEventListener("click", function () {
    search.value = "";
    activeFilter = "all";
    filters.forEach(function (filter) {
      var active = filter.dataset.filter === "all";
      filter.classList.toggle("active", active);
      filter.setAttribute("aria-pressed", String(active));
    });
    filterProviders();
    search.focus();
  });
  filterProviders();
  filtersReady = true;

  var nav = document.getElementById("nav");
  var navStuck = false;
  function updateNav() {
    var stuck = window.scrollY > 160;
    if (stuck !== navStuck) {
      navStuck = stuck;
      nav.classList.toggle("is-stuck", stuck);
    }
  }
  window.addEventListener("scroll", updateNav, { passive: true });
  updateNav();

  var hero = document.querySelector(".hero");
  var heroGlow = document.querySelector(".hero-glow");
  if (motion && finePointer.matches && hero && heroGlow) {
    var glowFrame = 0;
    var glowX = 0;
    var glowY = 0;
    hero.addEventListener("pointermove", function (event) {
      var bounds = hero.getBoundingClientRect();
      glowX = event.clientX - bounds.left;
      glowY = event.clientY - bounds.top;
      if (!glowFrame) glowFrame = window.requestAnimationFrame(function () {
        heroGlow.style.setProperty("--glow-x", glowX + "px");
        heroGlow.style.setProperty("--glow-y", glowY + "px");
        glowFrame = 0;
      });
      hero.classList.add("glowing");
    }, { passive: true });
    hero.addEventListener("pointerleave", function () { hero.classList.remove("glowing"); }, { passive: true });
  }

  if (motion && finePointer.matches) {
    document.querySelectorAll(".button-accent, .nav .button-ink").forEach(function (button) {
      button.classList.add("magnetic");
      button.addEventListener("pointermove", function (event) {
        var bounds = button.getBoundingClientRect();
        var dx = (event.clientX - bounds.left - bounds.width / 2) / bounds.width;
        var dy = (event.clientY - bounds.top - bounds.height / 2) / bounds.height;
        button.classList.add("pulling");
        button.style.translate = (dx * 10).toFixed(1) + "px " + (dy * 8).toFixed(1) + "px";
      }, { passive: true });
      button.addEventListener("pointerleave", function () {
        button.classList.remove("pulling");
        button.style.translate = "";
      }, { passive: true });
    });
  }

  var heroMarks = document.querySelector(".hero-marks");
  if (motion && heroMarks) {
    var seen = {};
    var track = document.createElement("div");
    track.className = "marquee-track";
    providers.forEach(function (provider) {
      var img = provider.querySelector("img");
      var src = img.getAttribute("src");
      if (seen[src]) return;
      seen[src] = true;
      var item = document.createElement("span");
      var mark = document.createElement("img");
      mark.src = src;
      mark.alt = "";
      mark.width = 20;
      mark.height = 20;
      item.appendChild(mark);
      item.appendChild(document.createTextNode(provider.querySelector("span").textContent));
      track.appendChild(item);
    });
    var echo = track.cloneNode(true);
    echo.setAttribute("aria-hidden", "true");
    heroMarks.textContent = "";
    heroMarks.classList.add("marquee");
    heroMarks.setAttribute("aria-label", "Supported providers: " + Object.keys(seen).length + " tools");
    heroMarks.appendChild(track);
    heroMarks.appendChild(echo);
    window.requestAnimationFrame(function () {
      heroMarks.style.setProperty("--marquee-duration", Math.max(30, track.scrollWidth / 45).toFixed(1) + "s");
    });
  }

  var ticker = document.getElementById("sessionTicker");
  if (motion && ticker && "IntersectionObserver" in window) {
    var tickerLines = ["Watching local session", "Reading session.jsonl", "Phase: running tests", "Presence updated"];
    var tickerIndex = 0;
    var tickerTimer = 0;
    new IntersectionObserver(function (entries) {
      window.clearInterval(tickerTimer);
      if (!entries[0].isIntersecting) return;
      tickerTimer = window.setInterval(function () {
        tickerIndex = (tickerIndex + 1) % tickerLines.length;
        ticker.textContent = tickerLines[tickerIndex];
        ticker.classList.remove("swap");
        void ticker.offsetWidth;
        ticker.classList.add("swap");
      }, 2600);
    }).observe(ticker);
  }

  var steps = document.querySelectorAll(".install-steps li");
  if (motion && "IntersectionObserver" in window) {
    var stepObserver = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (!entry.isIntersecting) return;
        entry.target.classList.add("lit");
        stepObserver.unobserve(entry.target);
      });
    }, { rootMargin: "0px 0px -35% 0px" });
    steps.forEach(function (step) { stepObserver.observe(step); });
  } else {
    steps.forEach(function (step) { step.classList.add("lit"); });
  }
  var copyButton = document.getElementById("copyButton");
  copyButton.addEventListener("click", function () {
    var text = document.getElementById("buildCommands").textContent;
    var label = copyButton.querySelector("span");
    function showCopied() {
      copyButton.classList.add("copied");
      label.textContent = "Copied";
      window.setTimeout(function () {
        copyButton.classList.remove("copied");
        label.textContent = "Copy";
      }, 1800);
    }
    function showError() {
      label.textContent = "Select text";
      window.setTimeout(function () { label.textContent = "Copy"; }, 1800);
    }
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).then(showCopied, showError);
    } else {
      var textArea = document.createElement("textarea");
      textArea.value = text;
      textArea.style.position = "fixed";
      textArea.style.opacity = "0";
      document.body.appendChild(textArea);
      textArea.select();
      try {
        if (document.execCommand("copy")) showCopied();
        else showError();
      } catch (error) { showError(); }
      textArea.remove();
    }
  });

  var usageDemo = document.getElementById("usageDemo");
  if (usageDemo) {
    var periodMeta = {
      today: {
        chartTitle: "TODAY'S ACTIVITY",
        buckets: ["09h", "11h", "13h", "15h", "17h", "19h", "21h"],
        providers: {
          codex: {
            label: "OpenAI Codex (.jsonl & .zst)",
            models: [
              { model: "gpt-5.4", provider: "Codex", effort: "high", tokens: 890000, inTokens: 808000, cached: 642000, outTokens: 82000, cost: 1.03, calls: 21, time: "16m" },
              { model: "gpt-5.4-mini", provider: "Codex", effort: "medium", tokens: 260000, inTokens: 236000, cached: 178000, outTokens: 24000, cost: 0.30, calls: 8, time: "5m" }
            ],
            daily: [
              { tokens: 120000, cost: 0.14, calls: 3 },
              { tokens: 210000, cost: 0.24, calls: 5 },
              { tokens: 160000, cost: 0.18, calls: 4 },
              { tokens: 240000, cost: 0.28, calls: 6 },
              { tokens: 180000, cost: 0.21, calls: 5 },
              { tokens: 140000, cost: 0.16, calls: 4 },
              { tokens: 100000, cost: 0.12, calls: 2 }
            ]
          },
          "claude-code": {
            label: "Claude Code (inclusive cache)",
            models: [
              { model: "claude-sonnet-4-5", provider: "Claude Code", effort: "medium", tokens: 520000, inTokens: 472000, cached: 374000, outTokens: 48000, cost: 0.75, calls: 15, time: "—" },
              { model: "claude-opus-4-6", provider: "Claude Code", effort: "high", tokens: 170000, inTokens: 152000, cached: 114000, outTokens: 18000, cost: 0.29, calls: 5, time: "—" }
            ],
            daily: [
              { tokens: 70000, cost: 0.11, calls: 2 },
              { tokens: 130000, cost: 0.20, calls: 4 },
              { tokens: 90000, cost: 0.13, calls: 3 },
              { tokens: 150000, cost: 0.23, calls: 4 },
              { tokens: 110000, cost: 0.16, calls: 3 },
              { tokens: 80000, cost: 0.12, calls: 2 },
              { tokens: 60000, cost: 0.09, calls: 2 }
            ]
          },
          antigravity: {
            label: "Google Antigravity",
            models: [
              { model: "gemini-3.1-pro", provider: "Antigravity", effort: "high", tokens: 330000, inTokens: 299000, cached: 224000, outTokens: 31000, cost: 0.42, calls: 10, time: "—" },
              { model: "gemini-3-flash", provider: "Antigravity", effort: "", tokens: 90000, inTokens: 81000, cached: 59000, outTokens: 9000, cost: 0.10, calls: 3, time: "—" }
            ],
            daily: [
              { tokens: 40000, cost: 0.05, calls: 1 },
              { tokens: 80000, cost: 0.10, calls: 2 },
              { tokens: 60000, cost: 0.07, calls: 2 },
              { tokens: 90000, cost: 0.11, calls: 3 },
              { tokens: 70000, cost: 0.09, calls: 2 },
              { tokens: 50000, cost: 0.06, calls: 2 },
              { tokens: 30000, cost: 0.04, calls: 1 }
            ]
          },
          "gemini-cli": {
            label: "Gemini CLI",
            models: [
              { model: "gemini-2.5-pro", provider: "Gemini CLI", effort: "", tokens: 220000, inTokens: 198000, cached: 144000, outTokens: 22000, cost: 0.25, calls: 7, time: "—" }
            ],
            daily: [
              { tokens: 20000, cost: 0.02, calls: 1 },
              { tokens: 40000, cost: 0.05, calls: 1 },
              { tokens: 30000, cost: 0.03, calls: 1 },
              { tokens: 50000, cost: 0.06, calls: 2 },
              { tokens: 30000, cost: 0.03, calls: 1 },
              { tokens: 30000, cost: 0.04, calls: 1 },
              { tokens: 20000, cost: 0.02, calls: 0 }
            ]
          },
          opencode: {
            label: "OpenCode (SQLite + WAL)",
            models: [
              { model: "qwen4-max", provider: "OpenCode", effort: "", tokens: 170000, inTokens: 154000, cached: 104000, outTokens: 16000, cost: 0.16, calls: 5, time: "—" }
            ],
            daily: [
              { tokens: 20000, cost: 0.02, calls: 1 },
              { tokens: 30000, cost: 0.03, calls: 1 },
              { tokens: 20000, cost: 0.02, calls: 0 },
              { tokens: 40000, cost: 0.04, calls: 1 },
              { tokens: 30000, cost: 0.03, calls: 1 },
              { tokens: 20000, cost: 0.01, calls: 1 },
              { tokens: 10000, cost: 0.01, calls: 0 }
            ]
          }
        }
      },
      "7d": {
        chartTitle: "DAILY ACTIVITY",
        buckets: ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"],
        providers: {
          codex: {
            label: "OpenAI Codex (.jsonl & .zst)",
            models: [
              { model: "gpt-5.4", provider: "Codex", effort: "high", tokens: 4920000, inTokens: 4460000, cached: 3520000, outTokens: 460000, cost: 5.68, calls: 114, time: "1h 24m" },
              { model: "gpt-5.4-mini", provider: "Codex", effort: "medium", tokens: 1500000, inTokens: 1360000, cached: 1020000, outTokens: 140000, cost: 1.74, calls: 44, time: "24m" }
            ],
            daily: [
              { tokens: 710000, cost: 0.82, calls: 18 },
              { tokens: 1120000, cost: 1.29, calls: 28 },
              { tokens: 520000, cost: 0.60, calls: 13 },
              { tokens: 790000, cost: 0.91, calls: 19 },
              { tokens: 1290000, cost: 1.49, calls: 32 },
              { tokens: 1080000, cost: 1.25, calls: 26 },
              { tokens: 910000, cost: 1.06, calls: 22 }
            ]
          },
          "claude-code": {
            label: "Claude Code (inclusive cache)",
            models: [
              { model: "claude-sonnet-4-5", provider: "Claude Code", effort: "medium", tokens: 2910000, inTokens: 2640000, cached: 2090000, outTokens: 270000, cost: 4.18, calls: 84, time: "—" },
              { model: "claude-opus-4-6", provider: "Claude Code", effort: "high", tokens: 970000, inTokens: 870000, cached: 650000, outTokens: 100000, cost: 1.66, calls: 28, time: "—" }
            ],
            daily: [
              { tokens: 420000, cost: 0.64, calls: 12 },
              { tokens: 650000, cost: 0.98, calls: 19 },
              { tokens: 310000, cost: 0.46, calls: 9 },
              { tokens: 490000, cost: 0.74, calls: 14 },
              { tokens: 780000, cost: 1.18, calls: 22 },
              { tokens: 690000, cost: 1.03, calls: 20 },
              { tokens: 540000, cost: 0.81, calls: 16 }
            ]
          },
          antigravity: {
            label: "Google Antigravity",
            models: [
              { model: "gemini-3.1-pro", provider: "Antigravity", effort: "high", tokens: 1820000, inTokens: 1650000, cached: 1240000, outTokens: 170000, cost: 2.32, calls: 56, time: "—" },
              { model: "gemini-3-flash", provider: "Antigravity", effort: "", tokens: 520000, inTokens: 470000, cached: 340000, outTokens: 50000, cost: 0.59, calls: 18, time: "—" }
            ],
            daily: [
              { tokens: 260000, cost: 0.32, calls: 8 },
              { tokens: 380000, cost: 0.47, calls: 12 },
              { tokens: 190000, cost: 0.24, calls: 6 },
              { tokens: 310000, cost: 0.38, calls: 10 },
              { tokens: 460000, cost: 0.57, calls: 15 },
              { tokens: 410000, cost: 0.51, calls: 13 },
              { tokens: 330000, cost: 0.42, calls: 10 }
            ]
          },
          "gemini-cli": {
            label: "Gemini CLI",
            models: [
              { model: "gemini-2.5-pro", provider: "Gemini CLI", effort: "", tokens: 1210000, inTokens: 1090000, cached: 790000, outTokens: 120000, cost: 1.38, calls: 41, time: "—" }
            ],
            daily: [
              { tokens: 130000, cost: 0.15, calls: 4 },
              { tokens: 190000, cost: 0.22, calls: 6 },
              { tokens: 100000, cost: 0.11, calls: 4 },
              { tokens: 150000, cost: 0.17, calls: 5 },
              { tokens: 240000, cost: 0.27, calls: 8 },
              { tokens: 220000, cost: 0.25, calls: 8 },
              { tokens: 180000, cost: 0.21, calls: 6 }
            ]
          },
          opencode: {
            label: "OpenCode (SQLite + WAL)",
            models: [
              { model: "qwen4-max", provider: "OpenCode", effort: "", tokens: 950000, inTokens: 860000, cached: 580000, outTokens: 90000, cost: 0.87, calls: 27, time: "—" }
            ],
            daily: [
              { tokens: 100000, cost: 0.11, calls: 4 },
              { tokens: 140000, cost: 0.16, calls: 4 },
              { tokens: 70000, cost: 0.03, calls: 2 },
              { tokens: 100000, cost: 0.08, calls: 3 },
              { tokens: 170000, cost: 0.17, calls: 5 },
              { tokens: 210000, cost: 0.20, calls: 5 },
              { tokens: 160000, cost: 0.12, calls: 4 }
            ]
          }
        }
      },
      "30d": {
        chartTitle: "30-DAY ACTIVITY",
        buckets: ["Oct 24", "Oct 29", "Nov 03", "Nov 07", "Nov 11", "Nov 15", "Nov 19"],
        providers: {
          codex: {
            label: "OpenAI Codex (.jsonl & .zst)",
            models: [
              { model: "gpt-5.4", provider: "Codex", effort: "high", tokens: 18400000, inTokens: 16680000, cached: 13180000, outTokens: 1720000, cost: 21.24, calls: 428, time: "5h 18m" },
              { model: "gpt-5.4-mini", provider: "Codex", effort: "medium", tokens: 5600000, inTokens: 5080000, cached: 3810000, outTokens: 520000, cost: 6.48, calls: 162, time: "1h 28m" }
            ],
            daily: [
              { tokens: 2800000, cost: 3.23, calls: 69 },
              { tokens: 3400000, cost: 3.93, calls: 84 },
              { tokens: 3100000, cost: 3.58, calls: 76 },
              { tokens: 3900000, cost: 4.50, calls: 96 },
              { tokens: 3600000, cost: 4.16, calls: 88 },
              { tokens: 4100000, cost: 4.74, calls: 101 },
              { tokens: 3100000, cost: 3.58, calls: 76 }
            ]
          },
          "claude-code": {
            label: "Claude Code (inclusive cache)",
            models: [
              { model: "claude-sonnet-4-5", provider: "Claude Code", effort: "medium", tokens: 11200000, inTokens: 10160000, cached: 8040000, outTokens: 1040000, cost: 16.08, calls: 322, time: "—" },
              { model: "claude-opus-4-6", provider: "Claude Code", effort: "high", tokens: 3700000, inTokens: 3320000, cached: 2490000, outTokens: 380000, cost: 6.32, calls: 106, time: "—" }
            ],
            daily: [
              { tokens: 1800000, cost: 2.71, calls: 52 },
              { tokens: 2200000, cost: 3.31, calls: 63 },
              { tokens: 1900000, cost: 2.86, calls: 55 },
              { tokens: 2400000, cost: 3.61, calls: 69 },
              { tokens: 2100000, cost: 3.16, calls: 60 },
              { tokens: 2600000, cost: 3.91, calls: 74 },
              { tokens: 1900000, cost: 2.84, calls: 55 }
            ]
          },
          antigravity: {
            label: "Google Antigravity",
            models: [
              { model: "gemini-3.1-pro", provider: "Antigravity", effort: "high", tokens: 6900000, inTokens: 6250000, cached: 4690000, outTokens: 650000, cost: 8.79, calls: 212, time: "—" },
              { model: "gemini-3-flash", provider: "Antigravity", effort: "", tokens: 1950000, inTokens: 1760000, cached: 1270000, outTokens: 190000, cost: 2.21, calls: 68, time: "—" }
            ],
            daily: [
              { tokens: 1050000, cost: 1.31, calls: 33 },
              { tokens: 1250000, cost: 1.55, calls: 40 },
              { tokens: 1150000, cost: 1.43, calls: 36 },
              { tokens: 1450000, cost: 1.80, calls: 46 },
              { tokens: 1300000, cost: 1.62, calls: 41 },
              { tokens: 1500000, cost: 1.86, calls: 48 },
              { tokens: 1150000, cost: 1.43, calls: 36 }
            ]
          },
          "gemini-cli": {
            label: "Gemini CLI",
            models: [
              { model: "gemini-2.5-pro", provider: "Gemini CLI", effort: "", tokens: 4550000, inTokens: 4100000, cached: 2970000, outTokens: 450000, cost: 5.19, calls: 154, time: "—" }
            ],
            daily: [
              { tokens: 550000, cost: 0.63, calls: 19 },
              { tokens: 650000, cost: 0.74, calls: 22 },
              { tokens: 600000, cost: 0.68, calls: 20 },
              { tokens: 750000, cost: 0.86, calls: 25 },
              { tokens: 650000, cost: 0.74, calls: 22 },
              { tokens: 750000, cost: 0.86, calls: 26 },
              { tokens: 600000, cost: 0.68, calls: 20 }
            ]
          },
          opencode: {
            label: "OpenCode (SQLite + WAL)",
            models: [
              { model: "qwen4-max", provider: "OpenCode", effort: "", tokens: 3550000, inTokens: 3210000, cached: 2170000, outTokens: 340000, cost: 3.25, calls: 102, time: "—" }
            ],
            daily: [
              { tokens: 420000, cost: 0.38, calls: 12 },
              { tokens: 510000, cost: 0.47, calls: 15 },
              { tokens: 460000, cost: 0.42, calls: 13 },
              { tokens: 580000, cost: 0.53, calls: 17 },
              { tokens: 520000, cost: 0.48, calls: 15 },
              { tokens: 600000, cost: 0.55, calls: 17 },
              { tokens: 460000, cost: 0.42, calls: 13 }
            ]
          }
        }
      },
      all: {
        chartTitle: "MONTHLY ACTIVITY",
        buckets: ["May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov"],
        providers: {
          codex: {
            label: "OpenAI Codex (.jsonl & .zst)",
            models: [
              { model: "gpt-5.4", provider: "Codex", effort: "high", tokens: 46200000, inTokens: 41880000, cached: 33090000, outTokens: 4320000, cost: 53.35, calls: 1072, time: "13h 12m" },
              { model: "gpt-5.4-mini", provider: "Codex", effort: "medium", tokens: 14100000, inTokens: 12780000, cached: 9580000, outTokens: 1320000, cost: 16.36, calls: 414, time: "3h 46m" }
            ],
            daily: [
              { tokens: 5800000, cost: 6.70, calls: 143 },
              { tokens: 7100000, cost: 8.21, calls: 175 },
              { tokens: 8400000, cost: 9.71, calls: 207 },
              { tokens: 9200000, cost: 10.64, calls: 227 },
              { tokens: 9800000, cost: 11.33, calls: 241 },
              { tokens: 10400000, cost: 12.02, calls: 256 },
              { tokens: 9600000, cost: 11.10, calls: 237 }
            ]
          },
          "claude-code": {
            label: "Claude Code (inclusive cache)",
            models: [
              { model: "claude-sonnet-4-5", provider: "Claude Code", effort: "medium", tokens: 27400000, inTokens: 24860000, cached: 19680000, outTokens: 2540000, cost: 39.36, calls: 790, time: "—" },
              { model: "claude-opus-4-6", provider: "Claude Code", effort: "high", tokens: 9100000, inTokens: 8160000, cached: 6100000, outTokens: 940000, cost: 15.56, calls: 262, time: "—" }
            ],
            daily: [
              { tokens: 3800000, cost: 5.72, calls: 110 },
              { tokens: 4500000, cost: 6.77, calls: 130 },
              { tokens: 5100000, cost: 7.67, calls: 147 },
              { tokens: 5600000, cost: 8.43, calls: 161 },
              { tokens: 5800000, cost: 8.73, calls: 167 },
              { tokens: 6100000, cost: 9.18, calls: 176 },
              { tokens: 5600000, cost: 8.42, calls: 161 }
            ]
          },
          antigravity: {
            label: "Google Antigravity",
            models: [
              { model: "gemini-3.1-pro", provider: "Antigravity", effort: "high", tokens: 17100000, inTokens: 15500000, cached: 11650000, outTokens: 1600000, cost: 21.80, calls: 526, time: "—" },
              { model: "gemini-3-flash", provider: "Antigravity", effort: "", tokens: 4880000, inTokens: 4410000, cached: 3190000, outTokens: 470000, cost: 5.54, calls: 169, time: "—" }
            ],
            daily: [
              { tokens: 1980000, cost: 2.46, calls: 63 },
              { tokens: 2500000, cost: 3.11, calls: 79 },
              { tokens: 3100000, cost: 3.86, calls: 98 },
              { tokens: 3400000, cost: 4.23, calls: 108 },
              { tokens: 3600000, cost: 4.48, calls: 114 },
              { tokens: 3900000, cost: 4.85, calls: 123 },
              { tokens: 3500000, cost: 4.35, calls: 110 }
            ]
          },
          "gemini-cli": {
            label: "Gemini CLI",
            models: [
              { model: "gemini-2.5-pro", provider: "Gemini CLI", effort: "", tokens: 11380000, inTokens: 10250000, cached: 7430000, outTokens: 1130000, cost: 12.97, calls: 386, time: "—" }
            ],
            daily: [
              { tokens: 1180000, cost: 1.35, calls: 40 },
              { tokens: 1400000, cost: 1.60, calls: 47 },
              { tokens: 1600000, cost: 1.82, calls: 54 },
              { tokens: 1750000, cost: 1.99, calls: 59 },
              { tokens: 1850000, cost: 2.11, calls: 63 },
              { tokens: 1900000, cost: 2.17, calls: 65 },
              { tokens: 1700000, cost: 1.93, calls: 58 }
            ]
          },
          opencode: {
            label: "OpenCode (SQLite + WAL)",
            models: [
              { model: "qwen4-max", provider: "OpenCode", effort: "", tokens: 8920000, inTokens: 8070000, cached: 5450000, outTokens: 850000, cost: 8.17, calls: 254, time: "—" }
            ],
            daily: [
              { tokens: 920000, cost: 0.84, calls: 26 },
              { tokens: 1100000, cost: 1.01, calls: 31 },
              { tokens: 1250000, cost: 1.15, calls: 36 },
              { tokens: 1400000, cost: 1.28, calls: 40 },
              { tokens: 1450000, cost: 1.33, calls: 41 },
              { tokens: 1500000, cost: 1.37, calls: 43 },
              { tokens: 1300000, cost: 1.19, calls: 37 }
            ]
          }
        }
      }
    };
    var currentPeriod = "7d";
    var currentProvider = "all";
    var currentMetric = "tokens";

    var uTokens = document.getElementById("uTokens");
    var uTokensSub = document.getElementById("uTokensSub");
    var uCost = document.getElementById("uCost");
    var uCalls = document.getElementById("uCalls");
    var uCallsSub = document.getElementById("uCallsSub");
    var uCache = document.getElementById("uCache");
    var uCacheSub = document.getElementById("uCacheSub");
    var uChartLabel = document.getElementById("uChartLabel");
    var uBars = document.getElementById("uBars");
    var uModelList = document.getElementById("uModelList");

    function fmtTokens(n) {
      if (n >= 1e9) return (n / 1e9).toFixed(1) + "B";
      if (n >= 1e6) return (n / 1e6).toFixed(1) + "M";
      if (n >= 1e3) return Math.round(n / 1e3) + "K";
      return String(Math.round(n));
    }
    function fmtCost(n) {
      return "~$" + (n >= 100 ? n.toFixed(0) : n.toFixed(2));
    }

    function selectUsageView() {
      var period = periodMeta[currentPeriod] || periodMeta["7d"];
      if (currentProvider !== "all" && period.providers[currentProvider]) {
        var single = period.providers[currentProvider];
        return {
          chartTitle: period.chartTitle,
          label: single.label,
          buckets: period.buckets,
          models: single.models.slice().sort(function (a, b) { return b.tokens - a.tokens; }),
          daily: single.daily
        };
      }
      var keys = Object.keys(period.providers);
      var grouped = {};
      var aggDaily = period.buckets.map(function () { return { tokens: 0, cost: 0, calls: 0 }; });
      keys.forEach(function (k) {
        var p = period.providers[k];
        p.models.forEach(function (m) {
          var gKey = m.provider + "\0" + m.model + "\0" + m.effort;
          if (!grouped[gKey]) {
            grouped[gKey] = {
              model: m.model,
              provider: m.provider,
              effort: m.effort,
              tokens: 0,
              inTokens: 0,
              cached: 0,
              outTokens: 0,
              cost: 0,
              calls: 0,
              time: m.time
            };
          }
          var row = grouped[gKey];
          row.tokens += m.tokens;
          row.inTokens += m.inTokens;
          row.cached += m.cached;
          row.outTokens += m.outTokens;
          row.cost += m.cost;
          row.calls += m.calls;
        });
        p.daily.forEach(function (d, idx) {
          aggDaily[idx].tokens += d.tokens;
          aggDaily[idx].cost += d.cost;
          aggDaily[idx].calls += d.calls;
        });
      });
      var models = Object.keys(grouped).map(function (k) { return grouped[k]; });
      models.sort(function (a, b) { return b.tokens - a.tokens; });
      return {
        chartTitle: period.chartTitle,
        label: "Across " + keys.length + " harnesses",
        buckets: period.buckets,
        models: models,
        daily: aggDaily
      };
    }

    function renderUsageDemo() {
      var view = selectUsageView();
      var totalTokens = 0, totalIn = 0, totalOut = 0, totalCached = 0, totalCost = 0, totalCalls = 0;
      view.models.forEach(function (m) {
        totalTokens += m.tokens;
        totalIn += m.inTokens;
        totalOut += m.outTokens;
        totalCached += m.cached;
        totalCost += m.cost;
        totalCalls += m.calls;
      });
      var cachePct = totalIn > 0 ? (totalCached / totalIn) * 100 : 0;

      uTokens.textContent = fmtTokens(totalTokens);
      uTokensSub.textContent = fmtTokens(totalIn) + " in · " + fmtTokens(totalOut) + " out";
      uCost.textContent = fmtCost(totalCost);
      uCalls.textContent = String(totalCalls);
      uCallsSub.textContent = view.label;
      uCache.textContent = cachePct.toFixed(1) + "%";
      uCacheSub.textContent = fmtTokens(totalCached) + " cached input";
      uChartLabel.textContent = view.chartTitle + " · " + currentMetric.toUpperCase();

      var values = view.daily.map(function (d, idx) {
        return { day: view.buckets[idx], val: d[currentMetric] };
      });
      var maxVal = values.reduce(function (m, item) { return Math.max(m, item.val); }, 0) || 1;
      uBars.textContent = "";
      values.forEach(function (item) {
        var col = document.createElement("div");
        col.className = "usage-bar-col";
        var valEl = document.createElement("span");
        valEl.className = "usage-bar-val";
        valEl.textContent = currentMetric === "tokens" ? fmtTokens(item.val) : currentMetric === "cost" ? "$" + item.val.toFixed(item.val >= 10 ? 0 : 2) : String(item.val);
        var track = document.createElement("div");
        track.className = "usage-bar-track";
        var fill = document.createElement("div");
        fill.className = "usage-bar-fill";
        fill.style.height = item.val > 0 ? Math.max(8, Math.round((item.val / maxVal) * 100)) + "%" : "0%";
        track.appendChild(fill);
        var dayEl = document.createElement("span");
        dayEl.className = "usage-bar-day";
        dayEl.textContent = item.day;
        col.appendChild(valEl);
        col.appendChild(track);
        col.appendChild(dayEl);
        uBars.appendChild(col);
      });

      uModelList.textContent = "";
      view.models.forEach(function (m) {
        var li = document.createElement("li");
        li.className = "usage-model-row";
        var meta = document.createElement("div");
        meta.className = "usage-model-meta";
        var title = document.createElement("div");
        title.className = "usage-model-title";
        var strong = document.createElement("strong");
        strong.textContent = m.model;
        title.appendChild(strong);
        if (currentProvider === "all") {
          var pBadge = document.createElement("span");
          pBadge.className = "usage-badge provider";
          pBadge.textContent = m.provider;
          title.appendChild(pBadge);
        }
        if (m.effort) {
          var eBadge = document.createElement("span");
          eBadge.className = "usage-badge";
          eBadge.textContent = m.effort;
          title.appendChild(eBadge);
        }
        var sub = document.createElement("span");
        sub.className = "usage-model-sub";
        sub.textContent = m.calls + " calls · task time " + m.time;
        meta.appendChild(title);
        meta.appendChild(sub);

        var nums = document.createElement("div");
        nums.className = "usage-model-nums";
        var tok = document.createElement("strong");
        tok.textContent = fmtTokens(m.tokens);
        var cost = document.createElement("small");
        cost.textContent = fmtCost(m.cost);
        nums.appendChild(tok);
        nums.appendChild(cost);

        li.appendChild(meta);
        li.appendChild(nums);
        uModelList.appendChild(li);
      });
    }

    usageDemo.querySelectorAll("[data-period]").forEach(function (btn) {
      btn.addEventListener("click", function () {
        currentPeriod = btn.dataset.period;
        usageDemo.querySelectorAll("[data-period]").forEach(function (b) {
          var active = b === btn;
          b.classList.toggle("active", active);
          b.setAttribute("aria-pressed", String(active));
        });
        renderUsageDemo();
      });
    });
    usageDemo.querySelectorAll("[data-uprovider]").forEach(function (btn) {
      btn.addEventListener("click", function () {
        currentProvider = btn.dataset.uprovider;
        usageDemo.querySelectorAll("[data-uprovider]").forEach(function (b) {
          var active = b === btn;
          b.classList.toggle("active", active);
          b.setAttribute("aria-pressed", String(active));
        });
        renderUsageDemo();
      });
    });
    usageDemo.querySelectorAll("[data-metric]").forEach(function (btn) {
      btn.addEventListener("click", function () {
        currentMetric = btn.dataset.metric;
        usageDemo.querySelectorAll("[data-metric]").forEach(function (b) {
          var active = b === btn;
          b.classList.toggle("active", active);
          b.setAttribute("aria-pressed", String(active));
        });
        renderUsageDemo();
      });
    });
    renderUsageDemo();
  }

  document.getElementById("year").textContent = String(new Date().getFullYear());
})();
