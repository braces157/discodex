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
    var usageData = {
      all: {
        label: "Across 5 harnesses",
        models: [
          { model: "gpt-5.4", provider: "Codex", effort: "high", tokens: 6420000, inTokens: 5820000, cached: 4540000, outTokens: 600000, cost: 7.42, calls: 158, time: "1h 48m" },
          { model: "claude-sonnet-4-5", provider: "Claude Code", effort: "medium", tokens: 3880000, inTokens: 3510000, cached: 2740000, outTokens: 370000, cost: 5.84, calls: 112, time: "—" },
          { model: "gemini-3.1-pro", provider: "Antigravity", effort: "high", tokens: 2340000, inTokens: 2120000, cached: 1580000, outTokens: 220000, cost: 2.91, calls: 74, time: "—" },
          { model: "gemini-2.5-pro", provider: "Gemini CLI", effort: "", tokens: 1210000, inTokens: 1090000, cached: 790000, outTokens: 120000, cost: 1.38, calls: 41, time: "—" },
          { model: "qwen4-max", provider: "OpenCode", effort: "", tokens: 950000, inTokens: 860000, cached: 580000, outTokens: 90000, cost: 0.87, calls: 27, time: "—" }
        ],
        daily: [
          { day: "Thu", tokens: 1620000, cost: 2.04, calls: 46 },
          { day: "Fri", tokens: 2480000, cost: 3.12, calls: 69 },
          { day: "Sat", tokens: 1190000, cost: 1.44, calls: 34 },
          { day: "Sun", tokens: 1840000, cost: 2.28, calls: 51 },
          { day: "Mon", tokens: 2940000, cost: 3.68, calls: 82 },
          { day: "Tue", tokens: 2610000, cost: 3.24, calls: 72 },
          { day: "Wed", tokens: 2120000, cost: 2.62, calls: 58 }
        ]
      },
      codex: {
        label: "OpenAI Codex (.jsonl & .zst)",
        models: [
          { model: "gpt-5.4", provider: "Codex", effort: "high", tokens: 4920000, inTokens: 4460000, cached: 3520000, outTokens: 460000, cost: 5.68, calls: 114, time: "1h 24m" },
          { model: "gpt-5.4-mini", provider: "Codex", effort: "medium", tokens: 1500000, inTokens: 1360000, cached: 1020000, outTokens: 140000, cost: 1.74, calls: 44, time: "24m" }
        ],
        daily: [
          { day: "Thu", tokens: 710000, cost: 0.82, calls: 18 },
          { day: "Fri", tokens: 1120000, cost: 1.29, calls: 28 },
          { day: "Sat", tokens: 520000, cost: 0.60, calls: 13 },
          { day: "Sun", tokens: 790000, cost: 0.91, calls: 19 },
          { day: "Mon", tokens: 1290000, cost: 1.49, calls: 32 },
          { day: "Tue", tokens: 1080000, cost: 1.25, calls: 26 },
          { day: "Wed", tokens: 910000, cost: 1.06, calls: 22 }
        ]
      },
      "claude-code": {
        label: "Claude Code (inclusive cache)",
        models: [
          { model: "claude-sonnet-4-5", provider: "Claude Code", effort: "medium", tokens: 2910000, inTokens: 2640000, cached: 2090000, outTokens: 270000, cost: 4.18, calls: 84, time: "—" },
          { model: "claude-opus-4-6", provider: "Claude Code", effort: "high", tokens: 970000, inTokens: 870000, cached: 650000, outTokens: 100000, cost: 1.66, calls: 28, time: "—" }
        ],
        daily: [
          { day: "Thu", tokens: 420000, cost: 0.64, calls: 12 },
          { day: "Fri", tokens: 650000, cost: 0.98, calls: 19 },
          { day: "Sat", tokens: 310000, cost: 0.46, calls: 9 },
          { day: "Sun", tokens: 490000, cost: 0.74, calls: 14 },
          { day: "Mon", tokens: 780000, cost: 1.18, calls: 22 },
          { day: "Tue", tokens: 690000, cost: 1.03, calls: 20 },
          { day: "Wed", tokens: 540000, cost: 0.81, calls: 16 }
        ]
      },
      antigravity: {
        label: "Google Antigravity",
        models: [
          { model: "gemini-3.1-pro", provider: "Antigravity", effort: "high", tokens: 1820000, inTokens: 1650000, cached: 1240000, outTokens: 170000, cost: 2.32, calls: 56, time: "—" },
          { model: "gemini-3-flash", provider: "Antigravity", effort: "", tokens: 520000, inTokens: 470000, cached: 340000, outTokens: 50000, cost: 0.59, calls: 18, time: "—" }
        ],
        daily: [
          { day: "Thu", tokens: 260000, cost: 0.32, calls: 8 },
          { day: "Fri", tokens: 380000, cost: 0.47, calls: 12 },
          { day: "Sat", tokens: 190000, cost: 0.24, calls: 6 },
          { day: "Sun", tokens: 310000, cost: 0.38, calls: 10 },
          { day: "Mon", tokens: 460000, cost: 0.57, calls: 15 },
          { day: "Tue", tokens: 410000, cost: 0.51, calls: 13 },
          { day: "Wed", tokens: 330000, cost: 0.42, calls: 10 }
        ]
      },
      "gemini-cli": {
        label: "Gemini CLI",
        models: [
          { model: "gemini-2.5-pro", provider: "Gemini CLI", effort: "", tokens: 1210000, inTokens: 1090000, cached: 790000, outTokens: 120000, cost: 1.38, calls: 41, time: "—" }
        ],
        daily: [
          { day: "Thu", tokens: 130000, cost: 0.15, calls: 4 },
          { day: "Fri", tokens: 190000, cost: 0.22, calls: 6 },
          { day: "Sat", tokens: 100000, cost: 0.11, calls: 4 },
          { day: "Sun", tokens: 150000, cost: 0.17, calls: 5 },
          { day: "Mon", tokens: 240000, cost: 0.27, calls: 8 },
          { day: "Tue", tokens: 220000, cost: 0.25, calls: 8 },
          { day: "Wed", tokens: 180000, cost: 0.21, calls: 6 }
        ]
      },
      opencode: {
        label: "OpenCode (SQLite + WAL)",
        models: [
          { model: "qwen4-max", provider: "OpenCode", effort: "", tokens: 950000, inTokens: 860000, cached: 580000, outTokens: 90000, cost: 0.87, calls: 27, time: "—" }
        ],
        daily: [
          { day: "Thu", tokens: 100000, cost: 0.11, calls: 4 },
          { day: "Fri", tokens: 140000, cost: 0.16, calls: 4 },
          { day: "Sat", tokens: 70000, cost: 0.03, calls: 2 },
          { day: "Sun", tokens: 100000, cost: 0.08, calls: 3 },
          { day: "Mon", tokens: 170000, cost: 0.17, calls: 5 },
          { day: "Tue", tokens: 210000, cost: 0.20, calls: 5 },
          { day: "Wed", tokens: 160000, cost: 0.12, calls: 4 }
        ]
      }
    };
    var periodScale = { today: 0.18, "7d": 1, "30d": 3.8, all: 9.4 };
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

    function renderUsageDemo() {
      var scale = periodScale[currentPeriod] || 1;
      var data = usageData[currentProvider] || usageData.all;
      var totalTokens = 0, totalIn = 0, totalOut = 0, totalCached = 0, totalCost = 0, totalCalls = 0;
      data.models.forEach(function (m) {
        totalTokens += m.tokens * scale;
        totalIn += m.inTokens * scale;
        totalOut += m.outTokens * scale;
        totalCached += m.cached * scale;
        totalCost += m.cost * scale;
        totalCalls += Math.max(1, Math.round(m.calls * scale));
      });
      var cachePct = totalIn > 0 ? (totalCached / totalIn) * 100 : 0;

      uTokens.textContent = fmtTokens(totalTokens);
      uTokensSub.textContent = fmtTokens(totalIn) + " in · " + fmtTokens(totalOut) + " out";
      uCost.textContent = fmtCost(totalCost);
      uCalls.textContent = String(totalCalls);
      uCallsSub.textContent = data.label;
      uCache.textContent = cachePct.toFixed(1) + "%";
      uCacheSub.textContent = fmtTokens(totalCached) + " cached input";
      uChartLabel.textContent = "DAILY ACTIVITY · " + currentMetric.toUpperCase();

      var values = data.daily.map(function (d) {
        var v = d[currentMetric] * scale;
        return { day: d.day, val: currentMetric === "calls" ? Math.max(1, Math.round(v)) : v };
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
        fill.style.height = Math.max(8, Math.round((item.val / maxVal) * 100)) + "%";
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
      data.models.forEach(function (m) {
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
        sub.textContent = Math.max(1, Math.round(m.calls * scale)) + " calls · task time " + m.time;
        meta.appendChild(title);
        meta.appendChild(sub);

        var nums = document.createElement("div");
        nums.className = "usage-model-nums";
        var tok = document.createElement("strong");
        tok.textContent = fmtTokens(m.tokens * scale);
        var cost = document.createElement("small");
        cost.textContent = fmtCost(m.cost * scale);
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
