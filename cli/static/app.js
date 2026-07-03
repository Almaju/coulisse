// Studio client behaviors layered on top of htmx. Four jobs:
// (1) highlight the sidebar link matching the current page, re-run after
// every hx-boost navigation; (2) surface a toast on the result of any
// mutating (non-GET) request, so saves and failures get uniform feedback
// without per-form wiring; (3) drive the off-canvas sidebar on mobile;
// (4) the yaml-enc htmx extension used by every YAML editor form.

(function () {
  // Forms with hx-ext="yaml-enc" submit their `__yaml` textarea verbatim
  // as the request body with Content-Type: application/yaml — browsers
  // don't ship a YAML parser, and the server accepts YAML on the same
  // routes as JSON. An htmx extension is the only supported hook for
  // replacing the encoded body (htmx ignores foreign fields on the
  // configRequest detail).
  htmx.defineExtension('yaml-enc', {
    onEvent: function (name, evt) {
      if (name === 'htmx:configRequest') {
        evt.detail.headers['Content-Type'] = 'application/yaml';
      }
    },
    encodeParameters: function (xhr, parameters) {
      var value =
        typeof parameters.get === 'function'
          ? parameters.get('__yaml')
          : parameters['__yaml'];
      return value || '';
    },
  });

  function markActiveNav() {
    var path = window.location.pathname;
    var links = document.querySelectorAll('aside nav a[href^="/admin/"]');
    var best = null;
    var bestLen = -1;
    links.forEach(function (a) {
      var href = a.getAttribute('href');
      a.classList.remove('is-active');
      if ((path === href || path.indexOf(href + '/') === 0) && href.length > bestLen) {
        best = a;
        bestLen = href.length;
      }
    });
    if (best) {
      best.classList.add('is-active');
    }
  }

  function toast(message, ok) {
    var box = document.getElementById('toasts');
    if (!box) {
      return;
    }
    var el = document.createElement('div');
    el.className = 'toast ' + (ok ? 'toast-ok' : 'toast-err');
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

  document.addEventListener('click', function (evt) {
    var toggle = evt.target.closest('.nav-toggle');
    if (toggle) {
      document.body.classList.toggle('nav-open');
      return;
    }
    if (document.body.classList.contains('nav-open') && evt.target.closest('.sidebar a')) {
      document.body.classList.remove('nav-open');
    }
  });
})();
