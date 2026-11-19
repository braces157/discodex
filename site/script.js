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

  document.getElementById("year").textContent = String(new Date().getFullYear());
})();
