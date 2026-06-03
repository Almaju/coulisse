// Studio client behaviors layered on top of htmx + Tailwind (both via
// CDN). Two jobs: (1) highlight the sidebar link matching the current
// page, re-run after every hx-boost navigation; (2) surface a toast on
// the result of any mutating (non-GET) request, so saves and failures
// get uniform feedback without per-form wiring.

(function () {
  function markActiveNav() {
    var path = window.location.pathname;
    var links = document.querySelectorAll('aside nav a[href^="/admin/"]');
    var best = null;
    var bestLen = -1;
    links.forEach(function (a) {
      var href = a.getAttribute('href');
      a.classList.remove('bg-slate-800', 'text-slate-100');
      a.classList.add('text-slate-400');
      if ((path === href || path.indexOf(href + '/') === 0) && href.length > bestLen) {
        best = a;
        bestLen = href.length;
      }
    });
    if (best) {
      best.classList.add('bg-slate-800', 'text-slate-100');
      best.classList.remove('text-slate-400');
    }
  }

  function toast(message, ok) {
    var box = document.getElementById('toasts');
    if (!box) {
      return;
    }
    var el = document.createElement('div');
    el.className =
      'pointer-events-auto rounded-md px-4 py-2 text-sm font-medium shadow-lg ' +
      (ok ? 'bg-emerald-600 text-white' : 'bg-rose-600 text-white');
    el.textContent = message;
    box.appendChild(el);
    window.setTimeout(function () {
      el.style.transition = 'opacity .3s';
      el.style.opacity = '0';
      window.setTimeout(function () {
        el.remove();
      }, 300);
    }, ok ? 2500 : 6000);
  }

  document.addEventListener('DOMContentLoaded', markActiveNav);
  document.body.addEventListener('htmx:afterSettle', markActiveNav);

  document.body.addEventListener('htmx:afterRequest', function (evt) {
    var cfg = evt.detail.requestConfig || {};
    var verb = (cfg.verb || 'get').toLowerCase();
    if (verb === 'get') {
      return;
    }
    if (evt.detail.successful) {
      toast('Saved', true);
    } else if (evt.detail.xhr) {
      var msg = (evt.detail.xhr.responseText || 'Request failed').trim();
      if (!msg) {
        msg = 'Request failed';
      }
      if (msg.length > 200) {
        msg = msg.slice(0, 200) + '…';
      }
      toast(msg, false);
    }
  });
})();
