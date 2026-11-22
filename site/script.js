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
    var SVG_NS = "http://www.w3.org/2000/svg";
    var providerMeta = {
      codex: { name: "Codex", color: "#c5f277", shareBias: 1.0 },
      "claude-code": { name: "Claude Code", color: "#f5b971", shareBias: 0.62 },
      antigravity: { name: "Antigravity", color: "#7dd3fc", shareBias: 0.37 },
      "gemini-cli": { name: "Gemini CLI", color: "#a78bfa", shareBias: 0.19 },
      opencode: { name: "OpenCode", color: "#6ee7b7", shareBias: 0.15 }
    };
    var providerOrder = ["codex", "claude-code", "antigravity", "gemini-cli", "opencode"];
    var metricColors = { tokens: "#c5f277", cost: "#6ee7b7", calls: "#7dd3fc" };

    var periodMeta = {
      today: {
        isHourly: true,
        sessions: 6,
        buckets: ["00:00", "01:00", "02:00", "03:00", "04:00", "05:00", "06:00", "07:00", "08:00", "09:00", "10:00", "11:00", "12:00", "13:00", "14:00", "15:00", "16:00", "17:00", "18:00", "19:00", "20:00", "21:00", "22:00", "23:00"],
        hoverTitles: ["Today · 00:00–01:00", "Today · 01:00–02:00", "Today · 02:00–03:00", "Today · 03:00–04:00", "Today · 04:00–05:00", "Today · 05:00–06:00", "Today · 06:00–07:00", "Today · 07:00–08:00", "Today · 08:00–09:00", "Today · 09:00–10:00", "Today · 10:00–11:00", "Today · 11:00–12:00", "Today · 12:00–13:00", "Today · 13:00–14:00", "Today · 14:00–15:00", "Today · 15:00–16:00", "Today · 16:00–17:00", "Today · 17:00–18:00", "Today · 18:00–19:00", "Today · 19:00–20:00", "Today · 20:00–21:00", "Today · 21:00–22:00", "Today · 22:00–23:00", "Today · 23:00–00:00"],
        weights: [0, 0, 0, 0, 0, 0, 0, 0, 0.03, 0.08, 0.12, 0.10, 0.04, 0.07, 0.13, 0.15, 0.11, 0.06, 0.02, 0.04, 0.03, 0.02, 0, 0],
        providers: {
          codex: {
            models: [
              { model: "gpt-5.4", provider: "Codex", slug: "codex", effort: "high", tokens: 890000, inTokens: 808000, cached: 642000, cacheWrite: 0, outTokens: 82000, reasoning: 31000, cost: 1.03, saved: 1.44, calls: 21, timeMs: 960000 },
              { model: "gpt-5.4-mini", provider: "Codex", slug: "codex", effort: "medium", tokens: 260000, inTokens: 236000, cached: 178000, cacheWrite: 0, outTokens: 24000, reasoning: 6400, cost: 0.30, saved: 0.24, calls: 8, timeMs: 300000 }
            ]
          },
          "claude-code": {
            models: [
              { model: "claude-sonnet-4-5", provider: "Claude Code", slug: "claude-code", effort: "medium", tokens: 520000, inTokens: 472000, cached: 364000, cacheWrite: 38000, outTokens: 48000, reasoning: 14000, cost: 0.75, saved: 0.98, calls: 15, timeMs: 0 },
              { model: "claude-opus-4-6", provider: "Claude Code", slug: "claude-code", effort: "high", tokens: 170000, inTokens: 152000, cached: 108000, cacheWrite: 16000, outTokens: 18000, reasoning: 7200, cost: 0.29, saved: 0.49, calls: 5, timeMs: 0 }
            ]
          },
          antigravity: {
            models: [
              { model: "gemini-3.1-pro", provider: "Antigravity", slug: "antigravity", effort: "high", tokens: 330000, inTokens: 299000, cached: 224000, cacheWrite: 0, outTokens: 31000, reasoning: 11200, cost: 0.42, saved: 0.38, calls: 10, timeMs: 0 },
              { model: "gemini-3-flash", provider: "Antigravity", slug: "antigravity", effort: "", tokens: 90000, inTokens: 81000, cached: 59000, cacheWrite: 0, outTokens: 9000, reasoning: 0, cost: 0.10, saved: 0.08, calls: 3, timeMs: 0 }
            ]
          },
          "gemini-cli": {
            models: [
              { model: "gemini-2.5-pro", provider: "Gemini CLI", slug: "gemini-cli", effort: "", tokens: 220000, inTokens: 198000, cached: 144000, cacheWrite: 0, outTokens: 22000, reasoning: 6800, cost: 0.25, saved: 0.21, calls: 7, timeMs: 0 }
            ]
          },
          opencode: {
            models: [
              { model: "qwen4-max", provider: "OpenCode", slug: "opencode", effort: "", tokens: 170000, inTokens: 154000, cached: 104000, cacheWrite: 12000, outTokens: 16000, reasoning: 0, cost: 0.16, saved: 0.14, calls: 5, timeMs: 0 }
            ]
          }
        }
      },
      "7d": {
        isHourly: false,
        sessions: 28,
        buckets: ["Nov 16", "Nov 17", "Nov 18", "Nov 19", "Nov 20", "Nov 21", "Nov 22"],
        hoverTitles: ["Mon, Nov 16", "Tue, Nov 17", "Wed, Nov 18", "Thu, Nov 19", "Fri, Nov 20", "Sat, Nov 21", "Sun, Nov 22"],
        weights: [0.11, 0.17, 0.08, 0.13, 0.20, 0.17, 0.14],
        providers: {
          codex: {
            models: [
              { model: "gpt-5.4", provider: "Codex", slug: "codex", effort: "high", tokens: 4920000, inTokens: 4460000, cached: 3520000, cacheWrite: 0, outTokens: 460000, reasoning: 168000, cost: 5.68, saved: 7.92, calls: 114, timeMs: 5040000 },
              { model: "gpt-5.4-mini", provider: "Codex", slug: "codex", effort: "medium", tokens: 1500000, inTokens: 1360000, cached: 1020000, cacheWrite: 0, outTokens: 140000, reasoning: 34000, cost: 1.74, saved: 1.38, calls: 44, timeMs: 1440000 }
            ]
          },
          "claude-code": {
            models: [
              { model: "claude-sonnet-4-5", provider: "Claude Code", slug: "claude-code", effort: "medium", tokens: 2910000, inTokens: 2640000, cached: 2040000, cacheWrite: 220000, outTokens: 270000, reasoning: 76000, cost: 4.18, saved: 5.51, calls: 84, timeMs: 0 },
              { model: "claude-opus-4-6", provider: "Claude Code", slug: "claude-code", effort: "high", tokens: 970000, inTokens: 870000, cached: 620000, cacheWrite: 94000, outTokens: 100000, reasoning: 38000, cost: 1.66, saved: 2.79, calls: 28, timeMs: 0 }
            ]
          },
          antigravity: {
            models: [
              { model: "gemini-3.1-pro", provider: "Antigravity", slug: "antigravity", effort: "high", tokens: 1820000, inTokens: 1650000, cached: 1240000, cacheWrite: 0, outTokens: 170000, reasoning: 61000, cost: 2.32, saved: 2.11, calls: 56, timeMs: 0 },
              { model: "gemini-3-flash", provider: "Antigravity", slug: "antigravity", effort: "", tokens: 520000, inTokens: 470000, cached: 340000, cacheWrite: 0, outTokens: 50000, reasoning: 0, cost: 0.59, saved: 0.44, calls: 18, timeMs: 0 }
            ]
          },
          "gemini-cli": {
            models: [
              { model: "gemini-2.5-pro", provider: "Gemini CLI", slug: "gemini-cli", effort: "", tokens: 1210000, inTokens: 1090000, cached: 790000, cacheWrite: 0, outTokens: 120000, reasoning: 36000, cost: 1.38, saved: 1.15, calls: 41, timeMs: 0 }
            ]
          },
          opencode: {
            models: [
              { model: "qwen4-max", provider: "OpenCode", slug: "opencode", effort: "", tokens: 950000, inTokens: 860000, cached: 580000, cacheWrite: 68000, outTokens: 90000, reasoning: 0, cost: 0.87, saved: 0.78, calls: 27, timeMs: 0 }
            ]
          }
        }
      },
      "30d": {
        isHourly: false,
        sessions: 96,
        buckets: ["Oct 24", "Oct 27", "Oct 30", "Nov 02", "Nov 05", "Nov 08", "Nov 11", "Nov 14", "Nov 17", "Nov 20", "Nov 22"],
        hoverTitles: ["Sat, Oct 24", "Tue, Oct 27", "Fri, Oct 30", "Mon, Nov 2", "Thu, Nov 5", "Sun, Nov 8", "Wed, Nov 11", "Sat, Nov 14", "Tue, Nov 17", "Fri, Nov 20", "Sun, Nov 22"],
        weights: [0.06, 0.09, 0.08, 0.11, 0.10, 0.05, 0.12, 0.08, 0.11, 0.12, 0.08],
        providers: {
          codex: {
            models: [
              { model: "gpt-5.4", provider: "Codex", slug: "codex", effort: "high", tokens: 18400000, inTokens: 16680000, cached: 13180000, cacheWrite: 0, outTokens: 1720000, reasoning: 632000, cost: 21.24, saved: 29.65, calls: 428, timeMs: 19080000 },
              { model: "gpt-5.4-mini", provider: "Codex", slug: "codex", effort: "medium", tokens: 5600000, inTokens: 5080000, cached: 3810000, cacheWrite: 0, outTokens: 520000, reasoning: 128000, cost: 6.48, saved: 5.14, calls: 162, timeMs: 5280000 }
            ]
          },
          "claude-code": {
            models: [
              { model: "claude-sonnet-4-5", provider: "Claude Code", slug: "claude-code", effort: "medium", tokens: 11200000, inTokens: 10160000, cached: 7860000, cacheWrite: 840000, outTokens: 1040000, reasoning: 292000, cost: 16.08, saved: 21.22, calls: 322, timeMs: 0 },
              { model: "claude-opus-4-6", provider: "Claude Code", slug: "claude-code", effort: "high", tokens: 3700000, inTokens: 3320000, cached: 2380000, cacheWrite: 360000, outTokens: 380000, reasoning: 146000, cost: 6.32, saved: 10.71, calls: 106, timeMs: 0 }
            ]
          },
          antigravity: {
            models: [
              { model: "gemini-3.1-pro", provider: "Antigravity", slug: "antigravity", effort: "high", tokens: 6900000, inTokens: 6250000, cached: 4690000, cacheWrite: 0, outTokens: 650000, reasoning: 230000, cost: 8.79, saved: 7.97, calls: 212, timeMs: 0 },
              { model: "gemini-3-flash", provider: "Antigravity", slug: "antigravity", effort: "", tokens: 1950000, inTokens: 1760000, cached: 1270000, cacheWrite: 0, outTokens: 190000, reasoning: 0, cost: 2.21, saved: 1.65, calls: 68, timeMs: 0 }
            ]
          },
          "gemini-cli": {
            models: [
              { model: "gemini-2.5-pro", provider: "Gemini CLI", slug: "gemini-cli", effort: "", tokens: 4550000, inTokens: 4100000, cached: 2970000, cacheWrite: 0, outTokens: 450000, reasoning: 135000, cost: 5.19, saved: 4.31, calls: 154, timeMs: 0 }
            ]
          },
          opencode: {
            models: [
              { model: "qwen4-max", provider: "OpenCode", slug: "opencode", effort: "", tokens: 3550000, inTokens: 3210000, cached: 2170000, cacheWrite: 254000, outTokens: 340000, reasoning: 0, cost: 3.25, saved: 2.93, calls: 102, timeMs: 0 }
            ]
          }
        }
      },
      all: {
        isHourly: false,
        truncated: true,
        sessions: 244,
        buckets: ["Oct 24", "Oct 27", "Oct 30", "Nov 02", "Nov 05", "Nov 08", "Nov 11", "Nov 14", "Nov 17", "Nov 20", "Nov 22"],
        hoverTitles: ["Sat, Oct 24", "Tue, Oct 27", "Fri, Oct 30", "Mon, Nov 2", "Thu, Nov 5", "Sun, Nov 8", "Wed, Nov 11", "Sat, Nov 14", "Tue, Nov 17", "Fri, Nov 20", "Sun, Nov 22"],
        weights: [0.06, 0.08, 0.09, 0.10, 0.11, 0.07, 0.11, 0.09, 0.10, 0.11, 0.08],
        providers: {
          codex: {
            models: [
              { model: "gpt-5.4", provider: "Codex", slug: "codex", effort: "high", tokens: 46200000, inTokens: 41880000, cached: 33090000, cacheWrite: 0, outTokens: 4320000, reasoning: 1580000, cost: 53.35, saved: 74.45, calls: 1072, timeMs: 47520000 },
              { model: "gpt-5.4-mini", provider: "Codex", slug: "codex", effort: "medium", tokens: 14100000, inTokens: 12780000, cached: 9580000, cacheWrite: 0, outTokens: 1320000, reasoning: 324000, cost: 16.36, saved: 12.93, calls: 414, timeMs: 13560000 }
            ]
          },
          "claude-code": {
            models: [
              { model: "claude-sonnet-4-5", provider: "Claude Code", slug: "claude-code", effort: "medium", tokens: 27400000, inTokens: 24860000, cached: 19240000, cacheWrite: 2060000, outTokens: 2540000, reasoning: 710000, cost: 39.36, saved: 51.95, calls: 790, timeMs: 0 },
              { model: "claude-opus-4-6", provider: "Claude Code", slug: "claude-code", effort: "high", tokens: 9100000, inTokens: 8160000, cached: 5850000, cacheWrite: 880000, outTokens: 940000, reasoning: 360000, cost: 15.56, saved: 26.32, calls: 262, timeMs: 0 }
            ]
          },
          antigravity: {
            models: [
              { model: "gemini-3.1-pro", provider: "Antigravity", slug: "antigravity", effort: "high", tokens: 17100000, inTokens: 15500000, cached: 11650000, cacheWrite: 0, outTokens: 1600000, reasoning: 565000, cost: 21.80, saved: 19.80, calls: 526, timeMs: 0 },
              { model: "gemini-3-flash", provider: "Antigravity", slug: "antigravity", effort: "", tokens: 4880000, inTokens: 4410000, cached: 3190000, cacheWrite: 0, outTokens: 470000, reasoning: 0, cost: 5.54, saved: 4.15, calls: 169, timeMs: 0 }
            ]
          },
          "gemini-cli": {
            models: [
              { model: "gemini-2.5-pro", provider: "Gemini CLI", slug: "gemini-cli", effort: "", tokens: 11380000, inTokens: 10250000, cached: 7430000, cacheWrite: 0, outTokens: 1130000, reasoning: 340000, cost: 12.97, saved: 10.77, calls: 386, timeMs: 0 }
            ]
          },
          opencode: {
            models: [
              { model: "qwen4-max", provider: "OpenCode", slug: "opencode", effort: "", tokens: 8920000, inTokens: 8070000, cached: 5450000, cacheWrite: 640000, outTokens: 850000, reasoning: 0, cost: 8.17, saved: 7.35, calls: 254, timeMs: 0 }
            ]
          }
        }
      }
    };

    var currentPeriod = "7d";
    var currentProvider = "all";
    var currentMetric = "tokens";
    var currentStyle = "bar";
    var currentStack = "mix";
    var currentHeatTab = "calendar";
    var currentSort = "tokens";
    var isWide = true;
    var hoveredIdx = -1;
    var expandedModelKey = "codex:gpt-5.4:high";

    var usageLayout = document.getElementById("usageLayout");
    var uLayoutToggle = document.getElementById("uLayoutToggle");
    var uLayoutLabel = document.getElementById("uLayoutLabel");
    var uLayoutIcon = document.getElementById("uLayoutIcon");
    var uTokens = document.getElementById("uTokens");
    var uTokensSub = document.getElementById("uTokensSub");
    var uDelta = document.getElementById("uDelta");
    var uSparkTokens = document.getElementById("uSparkTokens");
    var uCost = document.getElementById("uCost");
    var uPerCall = document.getElementById("uPerCall");
    var uCostSub = document.getElementById("uCostSub");
    var uSparkCost = document.getElementById("uSparkCost");
    var uCalls = document.getElementById("uCalls");
    var uCache = document.getElementById("uCache");
    var uSaved = document.getElementById("uSaved");
    var uTaskTime = document.getElementById("uTaskTime");
    var uStackGroup = document.getElementById("uStackGroup");
    var uChartTotal = document.getElementById("uChartTotal");
    var uReadout = document.getElementById("uReadout");
    var uReadoutTitle = document.getElementById("uReadoutTitle");
    var uReadoutVal = document.getElementById("uReadoutVal");
    var uReadoutSubLeft = document.getElementById("uReadoutSubLeft");
    var uReadoutSubRight = document.getElementById("uReadoutSubRight");
    var uChartPlot = document.getElementById("uChartPlot");
    var uChartLegend = document.getElementById("uChartLegend");
    var uChartNote = document.getElementById("uChartNote");
    var uStreakBadge = document.getElementById("uStreakBadge");
    var uHeatmapGrid = document.getElementById("uHeatmapGrid");
    var uHeatmapReadout = document.getElementById("uHeatmapReadout");
    var uProviderBar = document.getElementById("uProviderBar");
    var uProviderChips = document.getElementById("uProviderChips");
    var uProviderHint = document.getElementById("uProviderHint");
    var uAvgPerCall = document.getElementById("uAvgPerCall");
    var uMixBar = document.getElementById("uMixBar");
    var uMixLegend = document.getElementById("uMixLegend");
    var uModelList = document.getElementById("uModelList");

    function fmtTokens(n) {
      if (n >= 1e9) return (n / 1e9).toFixed(2) + "B";
      if (n >= 1e6) return (n / 1e6).toFixed(2) + "M";
      if (n >= 1e3) return (n / 1e3).toFixed(1) + "K";
      return String(Math.round(n));
    }
    function fmtMoney(n) {
      if (n > 0 && n < 0.01) return "$" + n.toFixed(4);
      return "$" + n.toFixed(2);
    }
    function fmtDuration(ms) {
      if (!ms || ms <= 0) return "—";
      var sec = Math.floor(ms / 1000);
      if (sec >= 3600) return Math.floor(sec / 3600) + "h " + String(Math.floor((sec % 3600) / 60)).padStart(2, "0") + "m";
      if (sec >= 60) return Math.floor(sec / 60) + "m " + String(sec % 60).padStart(2, "0") + "s";
      return sec + "s";
    }
    function fmtMetric(metric, val) {
      return metric === "cost" ? fmtMoney(val) : fmtTokens(val);
    }
    function svgEl(tag, attrs) {
      var el = document.createElementNS(SVG_NS, tag);
      Object.keys(attrs || {}).forEach(function (k) { el.setAttribute(k, String(attrs[k])); });
      return el;
    }

    function periodTokenTotal(periodKey, provFilter) {
      var p = periodMeta[periodKey];
      var keys = provFilter === "all" ? providerOrder : [provFilter];
      var sum = 0;
      keys.forEach(function (k) {
        if (!p.providers[k]) return;
        p.providers[k].models.forEach(function (m) { sum += m.tokens; });
      });
      return sum;
    }

    function buildSeriesPoints(period, provKeys) {
      var weights = period.weights;
      var normFactors = {};
      provKeys.forEach(function (slug, pIdx) {
        var raw = weights.map(function (w, idx) {
          return w === 0 ? 0 : w * (0.86 + ((idx * 7 + pIdx * 13) % 9) * 0.035);
        });
        var sum = raw.reduce(function (a, b) { return a + b; }, 0) || 1;
        normFactors[slug] = raw.map(function (v) { return v / sum; });
      });
      return period.buckets.map(function (label, idx) {
        var pt = {
          label: label,
          hoverTitle: period.hoverTitles[idx],
          tokens: 0,
          inTokens: 0,
          fresh: 0,
          cached: 0,
          cacheWrite: 0,
          outTokens: 0,
          cost: 0,
          calls: 0,
          byProvider: []
        };
        provKeys.forEach(function (slug) {
          var pData = period.providers[slug];
          if (!pData) return;
          var f = normFactors[slug][idx];
          var pTok = 0, pIn = 0, pCached = 0, pWrite = 0, pOut = 0, pCost = 0, pCalls = 0;
          pData.models.forEach(function (m) {
            pTok += m.tokens * f;
            pIn += m.inTokens * f;
            pCached += m.cached * f;
            pWrite += m.cacheWrite * f;
            pOut += m.outTokens * f;
            pCost += m.cost * f;
            pCalls += m.calls * f;
          });
          var roundedTok = Math.round(pTok);
          if (roundedTok > 0) pt.byProvider.push({ slug: slug, tokens: roundedTok, color: providerMeta[slug].color });
          pt.tokens += roundedTok;
          pt.inTokens += Math.round(pIn);
          pt.cached += Math.round(pCached);
          pt.cacheWrite += Math.round(pWrite);
          pt.outTokens += Math.round(pOut);
          pt.cost += pCost;
          pt.calls += Math.round(pCalls);
        });
        pt.fresh = Math.max(0, pt.inTokens - pt.cached - pt.cacheWrite);
        return pt;
      });
    }

    function renderSparkline(container, values, color) {
      container.textContent = "";
      var max = values.reduce(function (m, v) { return Math.max(m, v); }, 0);
      var svg = svgEl("svg", { viewBox: "0 0 64 22", preserveAspectRatio: "none" });
      if (!values.length || max <= 0) {
        svg.appendChild(svgEl("line", { x1: 2, y1: 19, x2: 62, y2: 19, stroke: color, "stroke-opacity": 0.3, "stroke-width": 1.5 }));
        container.appendChild(svg);
        return;
      }
      var pts = values.map(function (v, i) {
        var x = values.length === 1 ? 32 : 2 + (i / (values.length - 1)) * 60;
        var y = 19 - (v / max) * 15;
        return [x, y];
      });
      var d = "M " + pts[0][0].toFixed(1) + " " + pts[0][1].toFixed(1);
      for (var i = 1; i < pts.length; i += 1) {
        var mx = ((pts[i - 1][0] + pts[i][0]) * 0.5).toFixed(1);
        d += " C " + mx + " " + pts[i - 1][1].toFixed(1) + ", " + mx + " " + pts[i][1].toFixed(1) + ", " + pts[i][0].toFixed(1) + " " + pts[i][1].toFixed(1);
      }
      var areaD = d + " L " + pts[pts.length - 1][0].toFixed(1) + " 21 L " + pts[0][0].toFixed(1) + " 21 Z";
      svg.appendChild(svgEl("path", { d: areaD, fill: color, "fill-opacity": 0.18 }));
      svg.appendChild(svgEl("path", { d: d, fill: "none", stroke: color, "stroke-width": 1.6, "stroke-linecap": "round" }));
      var last = pts[pts.length - 1];
      svg.appendChild(svgEl("circle", { cx: last[0].toFixed(1), cy: last[1].toFixed(1), r: 2.3, fill: color }));
      container.appendChild(svg);
    }

    function buildHeatmapData(provFilter) {
      var scale = provFilter === "all" ? 1.0 : (providerMeta[provFilter] ? providerMeta[provFilter].shareBias : 0.4);
      var end = new Date(2026, 10, 22);
      var start = new Date(end.getTime() - 83 * 86400000);
      var leadingEmpty = start.getDay();
      var dayNames = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
      var monthNames = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
      var cells = [];
      var maxTok = 1;
      var activeDays = 0;
      for (var i = 0; i < 84; i += 1) {
        var d = new Date(start.getTime() + i * 86400000);
        var dow = d.getDay();
        var seed = ((i * 37 + (provFilter.length * 17)) % 29) / 29;
        var active = i >= 70 || (dow !== 0 && seed > 0.22) || seed > 0.56;
        var baseTok = active ? Math.round((450000 + seed * 2400000) * (dow === 0 || dow === 6 ? 0.65 : 1.0) * scale) : 0;
        var calls = active ? Math.max(1, Math.round(baseTok / 36000)) : 0;
        var cost = active ? baseTok * 0.00000125 : 0;
        if (baseTok > maxTok) maxTok = baseTok;
        if (calls > 0) activeDays += 1;
        cells.push({
          dayName: dayNames[dow],
          pretty: monthNames[d.getMonth()] + " " + d.getDate(),
          tokens: baseTok,
          calls: calls,
          cost: cost
        });
      }
      var streak = 0;
      for (var j = cells.length - 1; j >= 0; j -= 1) {
        if (cells[j].calls > 0) streak += 1;
        else break;
      }
      var peakHours = [];
      var maxHour = 1;
      for (var dw = 0; dw < 7; dw += 1) {
        var row = [];
        for (var h = 0; h < 24; h += 1) {
          var v = 0;
          if (h >= 9 && h <= 22) {
            var curve = Math.sin(((h - 8) / 15) * Math.PI);
            var hSeed = 0.55 + (((dw * 11 + h * 7 + provFilter.length * 5) % 10) * 0.09);
            v = Math.round(curve * hSeed * (dw === 0 || dw === 6 ? 0.58 : 1.0) * 1450000 * scale);
          }
          if (v > maxHour) maxHour = v;
          row.push(v);
        }
        peakHours.push(row);
      }
      return { leadingEmpty: leadingEmpty, cells: cells, maxTok: maxTok, activeDays: activeDays, streak: streak, peakHours: peakHours, maxHour: maxHour };
    }

    function renderHeatmap() {
      var hm = buildHeatmapData(currentProvider);
      uStreakBadge.textContent = hm.streak + "d streak · " + hm.activeDays + "d active";
      uHeatmapReadout.textContent = "Hover any cell for breakdown";
      uHeatmapReadout.classList.remove("active");
      uHeatmapGrid.textContent = "";

      function setHeatReadout(text) {
        if (text) {
          uHeatmapReadout.textContent = text;
          uHeatmapReadout.classList.add("active");
        } else {
          uHeatmapReadout.textContent = "Hover any cell for breakdown";
          uHeatmapReadout.classList.remove("active");
        }
      }

      if (currentHeatTab === "calendar") {
        var wrap = document.createElement("div");
        wrap.className = "usage-heat-cal-wrap";
        var row = document.createElement("div");
        row.className = "usage-heat-cal";
        var daysAxis = document.createElement("div");
        daysAxis.className = "usage-heat-days";
        ["Sun", "Wed", "Sat"].forEach(function (lbl) {
          var s = document.createElement("span");
          s.textContent = lbl;
          daysAxis.appendChild(s);
        });
        row.appendChild(daysAxis);

        var numWeeks = Math.ceil((hm.leadingEmpty + hm.cells.length) / 7);
        var weeksEl = document.createElement("div");
        weeksEl.className = "usage-heat-weeks";
        weeksEl.style.gridTemplateColumns = "repeat(" + numWeeks + ", minmax(0, 1fr))";
        for (var w = 0; w < numWeeks; w += 1) {
          var col = document.createElement("div");
          col.className = "usage-heat-col";
          for (var d = 0; d < 7; d += 1) {
            var slot = w * 7 + d;
            var idx = slot - hm.leadingEmpty;
            var btn = document.createElement("button");
            btn.type = "button";
            btn.className = "usage-heat-cell";
            if (idx < 0 || idx >= hm.cells.length) {
              btn.classList.add("empty");
              btn.tabIndex = -1;
              btn.setAttribute("aria-hidden", "true");
            } else {
              var cell = hm.cells[idx];
              var ratio = cell.tokens / hm.maxTok;
              var bg = cell.calls === 0 ? "rgba(255,255,255,.045)"
                : ratio > 0.72 ? "#c5f277"
                : ratio > 0.45 ? "rgba(197,242,119,.68)"
                : ratio > 0.20 ? "rgba(197,242,119,.40)"
                : "rgba(197,242,119,.20)";
              btn.style.background = bg;
              var info = cell.dayName + ", " + cell.pretty + " · " + fmtTokens(cell.tokens) + " tok · " + fmtMoney(cell.cost) + " · " + cell.calls + (cell.calls === 1 ? " call" : " calls");
              btn.setAttribute("aria-label", info);
              (function (txt) {
                btn.addEventListener("mouseenter", function () { setHeatReadout(txt); });
                btn.addEventListener("focus", function () { setHeatReadout(txt); });
                btn.addEventListener("mouseleave", function () { setHeatReadout(null); });
                btn.addEventListener("blur", function () { setHeatReadout(null); });
              })(info);
            }
            col.appendChild(btn);
          }
          weeksEl.appendChild(col);
        }
        row.appendChild(weeksEl);
        wrap.appendChild(row);

        var foot = document.createElement("div");
        foot.className = "usage-heat-cal-foot";
        var f1 = document.createElement("span");
        f1.textContent = "12 weeks ago";
        var scaleEl = document.createElement("span");
        scaleEl.className = "usage-heat-scale";
        scaleEl.appendChild(document.createTextNode("Less"));
        ["rgba(255,255,255,.05)", "rgba(197,242,119,.20)", "rgba(197,242,119,.40)", "rgba(197,242,119,.68)", "#c5f277"].forEach(function (c) {
          var sw = document.createElement("i");
          sw.style.background = c;
          scaleEl.appendChild(sw);
        });
        scaleEl.appendChild(document.createTextNode("More"));
        var f3 = document.createElement("span");
        f3.textContent = "Today";
        foot.appendChild(f1);
        foot.appendChild(scaleEl);
        foot.appendChild(f3);
        wrap.appendChild(foot);
        uHeatmapGrid.appendChild(wrap);
      } else {
        var hWrap = document.createElement("div");
        hWrap.className = "usage-heat-hours";
        var dLabels = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        hm.peakHours.forEach(function (hours, dow) {
          var hRow = document.createElement("div");
          hRow.className = "usage-heat-hrow";
          var lbl = document.createElement("span");
          lbl.textContent = dLabels[dow];
          hRow.appendChild(lbl);
          var cellsWrap = document.createElement("div");
          cellsWrap.className = "usage-heat-hcells";
          hours.forEach(function (val, hr) {
            var btn = document.createElement("button");
            btn.type = "button";
            btn.className = "usage-heat-cell";
            var ratio = val / hm.maxHour;
            btn.style.background = val === 0 ? "rgba(255,255,255,.045)"
              : ratio > 0.72 ? "#6ee7b7"
              : ratio > 0.45 ? "rgba(110,231,183,.65)"
              : ratio > 0.20 ? "rgba(110,231,183,.38)"
              : "rgba(110,231,183,.18)";
            var info = dLabels[dow] + " " + String(hr).padStart(2, "0") + ":00 · " + fmtTokens(val) + " tok";
            btn.setAttribute("aria-label", info);
            btn.addEventListener("mouseenter", function () { setHeatReadout(info); });
            btn.addEventListener("focus", function () { setHeatReadout(info); });
            btn.addEventListener("mouseleave", function () { setHeatReadout(null); });
            btn.addEventListener("blur", function () { setHeatReadout(null); });
            cellsWrap.appendChild(btn);
          });
          hRow.appendChild(cellsWrap);
          hWrap.appendChild(hRow);
        });
        var hAxis = document.createElement("div");
        hAxis.className = "usage-heat-haxis";
        ["00h", "06h", "12h", "18h", "23h"].forEach(function (t) {
          var sp = document.createElement("span");
          sp.textContent = t;
          hAxis.appendChild(sp);
        });
        hWrap.appendChild(hAxis);
        uHeatmapGrid.appendChild(hWrap);
      }
    }

    function renderUsageDemo() {
      var period = periodMeta[currentPeriod] || periodMeta["7d"];
      var activeKeys = currentProvider === "all" ? providerOrder : [currentProvider];
      var models = [];
      activeKeys.forEach(function (k) {
        if (period.providers[k]) {
          period.providers[k].models.forEach(function (m) { models.push(m); });
        }
      });

      var totalTokens = 0, totalIn = 0, totalOut = 0, totalCached = 0, totalWrite = 0, totalReason = 0, totalCost = 0, totalSaved = 0, totalCalls = 0, totalTimeMs = 0;
      models.forEach(function (m) {
        totalTokens += m.tokens;
        totalIn += m.inTokens;
        totalOut += m.outTokens;
        totalCached += m.cached;
        totalWrite += m.cacheWrite;
        totalReason += m.reasoning;
        totalCost += m.cost;
        totalSaved += m.saved;
        totalCalls += m.calls;
        totalTimeMs += m.timeMs;
      });
      var totalFresh = Math.max(0, totalIn - totalCached - totalWrite);
      var cachePct = totalIn > 0 ? (totalCached / totalIn) * 100 : 0;

      // Provider shares across ALL providers for this period
      var providerTotals = providerOrder.map(function (slug) {
        var sum = 0;
        if (period.providers[slug]) {
          period.providers[slug].models.forEach(function (m) { sum += m.tokens; });
        }
        return { slug: slug, name: providerMeta[slug].name, color: providerMeta[slug].color, tokens: sum };
      });
      var allPeriodTokens = providerTotals.reduce(function (a, b) { return a + b.tokens; }, 0) || 1;
      uProviderBar.textContent = "";
      providerTotals.forEach(function (p) {
        if (p.tokens <= 0) return;
        var seg = document.createElement("i");
        seg.style.width = ((p.tokens / allPeriodTokens) * 100).toFixed(2) + "%";
        seg.style.background = p.color;
        if (currentProvider !== "all" && currentProvider !== p.slug) seg.style.opacity = "0.25";
        uProviderBar.appendChild(seg);
      });
      uProviderChips.querySelectorAll("[data-uprovider]").forEach(function (btn) {
        var slug = btn.dataset.uprovider;
        var active = slug === currentProvider;
        btn.classList.toggle("active", active);
        btn.setAttribute("aria-pressed", String(active));
        if (slug !== "all") {
          var item = providerTotals.find(function (p) { return p.slug === slug; });
          var pct = item ? Math.round((item.tokens / allPeriodTokens) * 100) : 0;
          var b = btn.querySelector("b");
          if (!b) {
            b = document.createElement("b");
            btn.appendChild(b);
          }
          b.textContent = pct + "%";
        }
      });
      uProviderHint.textContent = currentProvider === "all" ? "Click to filter" : "Filtered · " + providerMeta[currentProvider].name;

      // Hero Bento + Delta + Sparklines
      var points = buildSeriesPoints(period, activeKeys);
      uTokens.textContent = fmtTokens(totalTokens);
      uTokensSub.textContent = fmtTokens(totalIn) + " in · " + fmtTokens(totalOut) + " out";
      uCost.textContent = fmtMoney(totalCost);
      uPerCall.textContent = totalCalls > 0 ? fmtMoney(totalCost / totalCalls) + " / call" : "Base-rate estimate";
      var activeDays = currentPeriod === "today" ? 1 : currentPeriod === "7d" ? 7 : currentPeriod === "30d" ? 26 : 68;
      uCostSub.textContent = activeDays > 1 ? "~" + fmtMoney(totalCost / activeDays) + " / active day" : "Base-rate approximation";

      if (currentPeriod === "today") {
        var todayTok = periodTokenTotal("today", currentProvider);
        var weekAvg = periodTokenTotal("7d", currentProvider) / 7;
        var pctD = weekAvg > 0 ? Math.round(((todayTok - weekAvg) / weekAvg) * 100) : 0;
        uDelta.textContent = (pctD >= 0 ? "+" : "") + pctD + "% vs 7d avg";
        uDelta.className = "usage-delta " + (pctD >= 0 ? "positive" : "negative");
      } else if (currentPeriod === "7d") {
        var wkAvg = periodTokenTotal("7d", currentProvider) / 7;
        var moAvg = periodTokenTotal("30d", currentProvider) / 30;
        var pctW = moAvg > 0 ? Math.round(((wkAvg - moAvg) / moAvg) * 100) : 0;
        uDelta.textContent = (pctW >= 0 ? "+" : "") + pctW + "% vs 30d avg";
        uDelta.className = "usage-delta " + (pctW >= 0 ? "positive" : "negative");
      } else {
        var sess = Math.max(1, Math.round(period.sessions * (totalTokens / allPeriodTokens)));
        uDelta.textContent = sess + (sess === 1 ? " session" : " sessions");
        uDelta.className = "usage-delta neutral";
      }

      renderSparkline(uSparkTokens, points.map(function (pt) { return pt.tokens; }), "#c5f277");
      renderSparkline(uSparkCost, points.map(function (pt) { return pt.cost; }), "#6ee7b7");

      // Telemetry strip
      uCalls.textContent = fmtTokens(totalCalls);
      uCache.textContent = Math.round(cachePct) + "%";
      uSaved.textContent = fmtMoney(totalSaved);
      uTaskTime.textContent = fmtDuration(totalTimeMs);

      // Chart controls & readout
      var canStackByProvider = currentMetric === "tokens" && currentStyle === "bar" && currentProvider === "all";
      uStackGroup.hidden = !canStackByProvider;
      var effectiveStack = canStackByProvider ? currentStack : "mix";
      var metricColor = metricColors[currentMetric];
      uChartTotal.style.color = metricColor;

      var values = points.map(function (pt) { return pt[currentMetric]; });
      var totalMetric = values.reduce(function (a, b) { return a + b; }, 0);
      var maxRaw = values.reduce(function (m, v) { return Math.max(m, v); }, 0);
      var maxScale = maxRaw > 0 ? maxRaw * 1.12 : 1;
      var avgVal = values.length > 0 ? totalMetric / values.length : 0;
      uChartTotal.textContent = fmtMetric(currentMetric, totalMetric);

      function updateReadout(idx) {
        if (idx >= 0 && idx < points.length) {
          var pt = points[idx];
          var ptCache = pt.inTokens > 0 ? Math.round((pt.cached / pt.inTokens) * 100) : 0;
          uReadout.classList.add("active");
          uReadoutTitle.textContent = pt.hoverTitle;
          uReadoutVal.textContent = fmtMetric(currentMetric, pt[currentMetric]);
          uReadoutVal.style.color = metricColor;
          uReadoutSubLeft.textContent = "Fresh " + fmtTokens(pt.fresh) + " · Cache " + fmtTokens(pt.cached) + " · Out " + fmtTokens(pt.outTokens);
          uReadoutSubRight.textContent = fmtMoney(pt.cost) + " · " + pt.calls + (pt.calls === 1 ? " call" : " calls") + " · " + ptCache + "% cache";
        } else {
          uReadout.classList.remove("active");
          uReadoutTitle.textContent = "Period summary";
          uReadoutVal.textContent = maxRaw > 0 ? "Peak " + fmtMetric(currentMetric, maxRaw) + " · Avg " + fmtMetric(currentMetric, avgVal) : "No activity";
          uReadoutVal.style.color = "";
          uReadoutSubLeft.textContent = "Hover chart for exact breakdown";
          uReadoutSubRight.textContent = period.isHourly ? "24h local" : "";
        }
      }
      updateReadout(hoveredIdx);

      // Render chart plot
      uChartPlot.textContent = "";
      var stage = document.createElement("div");
      stage.className = "usage-plot-stage";
      var yAxis = document.createElement("div");
      yAxis.className = "usage-yaxis";
      [fmtMetric(currentMetric, maxScale), fmtMetric(currentMetric, maxScale * 0.5), "0"].forEach(function (lbl) {
        var sp = document.createElement("span");
        sp.textContent = lbl;
        yAxis.appendChild(sp);
      });
      stage.appendChild(yAxis);

      var inner = document.createElement("div");
      inner.className = "usage-plot-inner";
      var grid = document.createElement("div");
      grid.className = "usage-gridlines";
      grid.appendChild(document.createElement("i"));
      grid.appendChild(document.createElement("i"));
      grid.appendChild(document.createElement("i"));
      inner.appendChild(grid);

      if (avgVal > 0 && maxScale > 0) {
        var avgLine = document.createElement("div");
        avgLine.className = "usage-avg-line";
        avgLine.style.bottom = Math.min(98, (avgVal / maxScale) * 100).toFixed(1) + "%";
        avgLine.style.borderTopColor = metricColor;
        avgLine.style.opacity = "0.4";
        inner.appendChild(avgLine);
      }

      if (currentStyle === "bar") {
        var barsWrap = document.createElement("div");
        barsWrap.className = "usage-bars-wrap";
        var slotEls = [];
        points.forEach(function (pt, idx) {
          var val = values[idx];
          var slot = document.createElement("button");
          slot.type = "button";
          slot.className = "usage-bar-slot";
          slot.setAttribute("aria-label", pt.hoverTitle + ": " + fmtMetric(currentMetric, val));
          var stack = document.createElement("div");
          stack.className = "usage-bar-stack";
          var pctH = val > 0 ? Math.max(4, (val / maxScale) * 100) : 2;
          stack.style.height = pctH.toFixed(1) + "%";
          if (val <= 0) {
            stack.style.background = "rgba(255,255,255,.07)";
          } else if (currentMetric === "tokens") {
            var segs = effectiveStack === "providers"
              ? pt.byProvider.slice().reverse().map(function (p) { return { v: p.tokens, c: p.color }; })
              : [
                  { v: pt.outTokens, c: "#7dd3fc" },
                  { v: pt.cached, c: "#6ee7b7" },
                  { v: pt.fresh + pt.cacheWrite, c: "#c5f277" }
                ].filter(function (s) { return s.v > 0; });
            var sumSeg = segs.reduce(function (a, b) { return a + b.v; }, 0) || 1;
            segs.forEach(function (s) {
              var segEl = document.createElement("div");
              segEl.className = "usage-bar-seg";
              segEl.style.flexGrow = String(s.v / sumSeg);
              segEl.style.background = s.c;
              stack.appendChild(segEl);
            });
          } else {
            stack.style.background = metricColor;
          }
          slot.appendChild(stack);
          slot.addEventListener("mouseenter", function () {
            hoveredIdx = idx;
            slotEls.forEach(function (el, j) {
              el.classList.toggle("hovered", j === idx);
              el.classList.toggle("dimmed", j !== idx);
            });
            updateReadout(idx);
          });
          slot.addEventListener("focus", function () {
            hoveredIdx = idx;
            slotEls.forEach(function (el, j) {
              el.classList.toggle("hovered", j === idx);
              el.classList.toggle("dimmed", j !== idx);
            });
            updateReadout(idx);
          });
          slotEls.push(slot);
          barsWrap.appendChild(slot);
        });
        barsWrap.addEventListener("mouseleave", function () {
          hoveredIdx = -1;
          slotEls.forEach(function (el) { el.classList.remove("hovered", "dimmed"); });
          updateReadout(-1);
        });
        inner.appendChild(barsWrap);
      } else {
        var W = 320, H = 114;
        var svg = svgEl("svg", { class: "usage-area-svg", viewBox: "0 0 " + W + " " + H, preserveAspectRatio: "none" });
        var n = values.length;
        var slotW = W / n;
        var coords = values.map(function (v, i) {
          var x = (i + 0.5) * slotW;
          var y = (H - 3) - (v / maxScale) * (H - 8);
          return [x, y];
        });
        if (coords.length >= 2) {
          var pathD = "M " + coords[0][0].toFixed(1) + " " + coords[0][1].toFixed(1);
          for (var c = 1; c < coords.length; c += 1) {
            var mx = ((coords[c - 1][0] + coords[c][0]) * 0.5).toFixed(1);
            pathD += " C " + mx + " " + coords[c - 1][1].toFixed(1) + ", " + mx + " " + coords[c][1].toFixed(1) + ", " + coords[c][0].toFixed(1) + " " + coords[c][1].toFixed(1);
          }
          var fillD = pathD + " L " + coords[coords.length - 1][0].toFixed(1) + " " + H + " L " + coords[0][0].toFixed(1) + " " + H + " Z";
          svg.appendChild(svgEl("path", { d: fillD, fill: metricColor, "fill-opacity": 0.2 }));
          svg.appendChild(svgEl("path", { d: pathD, fill: "none", stroke: metricColor, "stroke-width": 2, "stroke-linecap": "round" }));
        }
        var crossLine = svgEl("line", { x1: 0, y1: 0, x2: 0, y2: H, stroke: "rgba(255,255,255,.26)", "stroke-width": 1, visibility: "hidden" });
        var hoverDot = svgEl("circle", { cx: 0, cy: 0, r: 4, fill: metricColor, stroke: "#0d0f12", "stroke-width": 1.5, visibility: "hidden" });
        svg.appendChild(crossLine);
        svg.appendChild(hoverDot);
        inner.appendChild(svg);

        var hitsWrap = document.createElement("div");
        hitsWrap.className = "usage-area-hits";
        points.forEach(function (pt, idx) {
          var hit = document.createElement("button");
          hit.type = "button";
          hit.className = "usage-area-hit";
          hit.setAttribute("aria-label", pt.hoverTitle + ": " + fmtMetric(currentMetric, values[idx]));
          function activate() {
            hoveredIdx = idx;
            crossLine.setAttribute("x1", coords[idx][0].toFixed(1));
            crossLine.setAttribute("x2", coords[idx][0].toFixed(1));
            crossLine.setAttribute("visibility", "visible");
            hoverDot.setAttribute("cx", coords[idx][0].toFixed(1));
            hoverDot.setAttribute("cy", coords[idx][1].toFixed(1));
            hoverDot.setAttribute("visibility", "visible");
            updateReadout(idx);
          }
          hit.addEventListener("mouseenter", activate);
          hit.addEventListener("focus", activate);
          hitsWrap.appendChild(hit);
        });
        hitsWrap.addEventListener("mouseleave", function () {
          hoveredIdx = -1;
          crossLine.setAttribute("visibility", "hidden");
          hoverDot.setAttribute("visibility", "hidden");
          updateReadout(-1);
        });
        inner.appendChild(hitsWrap);
      }

      stage.appendChild(inner);
      uChartPlot.appendChild(stage);

      var xAxis = document.createElement("div");
      xAxis.className = "usage-xaxis";
      var xLabels = period.isHourly
        ? ["00:00", "06:00", "12:00", "18:00", "23:00"]
        : [period.buckets[0], period.buckets[Math.floor(period.buckets.length / 2)], period.buckets[period.buckets.length - 1]];
      xLabels.forEach(function (lbl) {
        var sp = document.createElement("span");
        sp.textContent = lbl;
        xAxis.appendChild(sp);
      });
      uChartPlot.appendChild(xAxis);

      // Legend & note
      uChartLegend.textContent = "";
      if (currentStyle === "bar" && currentMetric === "tokens") {
        var legItems = effectiveStack === "providers"
          ? providerOrder.map(function (slug) { return { label: providerMeta[slug].name, color: providerMeta[slug].color }; })
          : [
              { label: "Fresh In", color: "#c5f277" },
              { label: "Cached", color: "#6ee7b7" },
              { label: "Output", color: "#7dd3fc" }
            ];
        legItems.forEach(function (it) {
          var el = document.createElement("span");
          el.className = "usage-legend-item";
          var sw = document.createElement("i");
          sw.style.background = it.color;
          el.appendChild(sw);
          el.appendChild(document.createTextNode(it.label));
          uChartLegend.appendChild(el);
        });
      }
      uChartNote.textContent = period.isHourly ? "24h local timeline" : period.truncated ? "Latest 30d · Full range in totals" : "";

      // Token Mix card
      uAvgPerCall.textContent = totalCalls > 0 ? fmtTokens(Math.round(totalTokens / totalCalls)) + " tok / call" : "—";
      var mixTotal = Math.max(1, totalFresh + totalCached + totalWrite + totalOut);
      uMixBar.textContent = "";
      [
        { v: totalFresh, c: "#c5f277" },
        { v: totalCached, c: "#6ee7b7" },
        { v: totalWrite, c: "#f5b971" },
        { v: totalOut, c: "#7dd3fc" }
      ].forEach(function (seg) {
        if (seg.v <= 0) return;
        var el = document.createElement("i");
        el.style.width = Math.max(1.5, (seg.v / mixTotal) * 100).toFixed(2) + "%";
        el.style.background = seg.c;
        uMixBar.appendChild(el);
      });
      uMixLegend.textContent = "";
      [
        { lbl: "Fresh In", v: totalFresh, c: "#c5f277", out: false },
        { lbl: "Cache Read", v: totalCached, c: "#6ee7b7", out: false },
        { lbl: "Cache Write", v: totalWrite, c: "#f5b971", out: false },
        { lbl: "Output", v: totalOut, c: "#7dd3fc", out: false },
        { lbl: "Reasoning", v: totalReason, c: "#a78bfa", out: true }
      ].forEach(function (row) {
        var item = document.createElement("div");
        item.className = "usage-mix-item";
        var lbl = document.createElement("span");
        lbl.className = "usage-mix-lbl";
        var dot = document.createElement("i");
        dot.style.background = row.c;
        lbl.appendChild(dot);
        lbl.appendChild(document.createTextNode(row.lbl));
        var val = document.createElement("span");
        val.className = "usage-mix-val";
        var denom = row.out ? Math.max(1, totalOut) : mixTotal;
        var pct = Math.round((row.v / denom) * 100);
        val.textContent = fmtTokens(row.v) + " (" + pct + (row.out ? "% out)" : "%)");
        item.appendChild(lbl);
        item.appendChild(val);
        uMixLegend.appendChild(item);
      });

      // Sortable Models list with inline expandable drawer
      var sortedModels = models.slice().sort(function (a, b) {
        if (currentSort === "cost") return b.cost - a.cost || b.tokens - a.tokens;
        if (currentSort === "calls") return b.calls - a.calls || b.tokens - a.tokens;
        if (currentSort === "cache") {
          var pa = a.inTokens > 0 ? a.cached / a.inTokens : 0;
          var pb = b.inTokens > 0 ? b.cached / b.inTokens : 0;
          return pb - pa || b.tokens - a.tokens;
        }
        return b.tokens - a.tokens || b.calls - a.calls;
      });
      uModelList.textContent = "";
      sortedModels.forEach(function (m) {
        var key = m.slug + ":" + m.model + ":" + m.effort;
        var expanded = expandedModelKey === key;
        var li = document.createElement("li");
        li.className = "usage-model-item" + (expanded ? " expanded" : "");
        var btn = document.createElement("button");
        btn.type = "button";
        btn.className = "usage-model-row";
        btn.setAttribute("aria-expanded", String(expanded));

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
        var mCachePct = m.inTokens > 0 ? Math.round((m.cached / m.inTokens) * 100) : 0;
        var sub = document.createElement("span");
        sub.className = "usage-model-sub";
        sub.textContent = m.calls + " calls · " + mCachePct + "% cache" + (m.timeMs > 0 ? " · " + fmtDuration(m.timeMs) : "");
        meta.appendChild(title);
        meta.appendChild(sub);

        var nums = document.createElement("div");
        nums.className = "usage-model-nums";
        var tok = document.createElement("strong");
        tok.textContent = fmtTokens(m.tokens);
        var cost = document.createElement("small");
        cost.textContent = fmtMoney(m.cost);
        nums.appendChild(tok);
        nums.appendChild(cost);

        btn.appendChild(meta);
        btn.appendChild(nums);
        btn.addEventListener("click", function () {
          expandedModelKey = expandedModelKey === key ? "" : key;
          renderUsageDemo();
        });
        li.appendChild(btn);

        if (expanded) {
          var drawer = document.createElement("div");
          drawer.className = "usage-model-drawer";
          var freshIn = Math.max(0, m.inTokens - m.cached - m.cacheWrite);
          [
            { lbl: "Fresh In", val: fmtTokens(freshIn), cls: "" },
            { lbl: "Cache Read", val: fmtTokens(m.cached), cls: "emerald" },
            { lbl: "Output", val: fmtTokens(m.outTokens), cls: "" },
            { lbl: "Cache Hit", val: mCachePct + "%", cls: "lime" },
            { lbl: "Cache Saved", val: fmtMoney(m.saved), cls: "emerald" },
            { lbl: "Task Time", val: fmtDuration(m.timeMs), cls: "" }
          ].forEach(function (st) {
            var box = document.createElement("div");
            box.className = "usage-drawer-stat";
            var sp = document.createElement("span");
            sp.textContent = st.lbl;
            var sv = document.createElement("strong");
            if (st.cls) sv.className = st.cls;
            sv.textContent = st.val;
            box.appendChild(sp);
            box.appendChild(sv);
            drawer.appendChild(box);
          });
          li.appendChild(drawer);
        }
        uModelList.appendChild(li);
      });

      renderHeatmap();
    }

    function wireGroup(selector, onPick) {
      usageDemo.querySelectorAll(selector).forEach(function (btn) {
        btn.addEventListener("click", function () {
          usageDemo.querySelectorAll(selector).forEach(function (b) {
            var active = b === btn;
            b.classList.toggle("active", active);
            b.setAttribute("aria-pressed", String(active));
          });
          onPick(btn);
          renderUsageDemo();
        });
      });
    }

    wireGroup("[data-period]", function (btn) { currentPeriod = btn.dataset.period; hoveredIdx = -1; });
    wireGroup("[data-metric]", function (btn) { currentMetric = btn.dataset.metric; hoveredIdx = -1; });
    wireGroup("[data-style]", function (btn) { currentStyle = btn.dataset.style; hoveredIdx = -1; });
    wireGroup("[data-stack]", function (btn) { currentStack = btn.dataset.stack; });
    wireGroup("[data-heat]", function (btn) { currentHeatTab = btn.dataset.heat; });
    wireGroup("[data-sort]", function (btn) { currentSort = btn.dataset.sort; });

    uProviderChips.querySelectorAll("[data-uprovider]").forEach(function (btn) {
      btn.addEventListener("click", function () {
        currentProvider = btn.dataset.uprovider;
        hoveredIdx = -1;
        renderUsageDemo();
      });
    });

    if (uLayoutToggle && usageLayout) {
      uLayoutToggle.addEventListener("click", function () {
        isWide = !isWide;
        usageLayout.classList.toggle("is-wide", isWide);
        uLayoutToggle.classList.toggle("active", isWide);
        uLayoutToggle.setAttribute("aria-pressed", String(isWide));
        uLayoutLabel.textContent = isWide ? "Compact" : "Wide";
        uLayoutIcon.setAttribute("href", isWide ? "#i-collapse" : "#i-expand");
      });
    }

    renderUsageDemo();
  }

  document.getElementById("year").textContent = String(new Date().getFullYear());
})();
