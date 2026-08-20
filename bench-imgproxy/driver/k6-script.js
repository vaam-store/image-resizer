// Load driver for the emgr-vs-imgproxy benchmark. Runs inside the
// `grafana/k6` container, on the same `bench` docker network as the
// services under test -- see ../compose.yaml and ../README.md.
//
// The URL shape for each engine lives entirely in the `urlBuilders` map
// below, keyed by ENGINE. emgr's URL API is being rewritten right now
// (imgproxy-compatible signed paths, see adr/0002-url-api-shape.md at the
// repo root) -- this driver targets emgr's CURRENT query-parameter API
// (`GET /api/images/resize?url=...&width=...&height=...&format=...`, see
// openapi.yaml at the time this was written). When the rewrite lands,
// add a new entry to `urlBuilders` for the new shape rather than editing
// the emgr builder in place, so both remain runnable side by side during
// the transition.
import http from 'k6/http';
import { Counter, Trend } from 'k6/metrics';
import { check } from 'k6';

// ---------------------------------------------------------------------
// Configuration (all via env vars, set by ../driver/run.sh per scenario)
// ---------------------------------------------------------------------

const ENGINE = __ENV.ENGINE || 'emgr'; // 'emgr' | 'imgproxy'
const SCENARIO = __ENV.SCENARIO || 'cold'; // 'cold' | 'warm'
const ENGINE_BASE_URL = __ENV.ENGINE_BASE_URL || 'http://origin:3000';
// Where the *proxy itself* fetches source images from -- NOT necessarily
// where k6 fetches the proxy from. Deliberately different per engine; see
// the compose.yaml header comment and README.md for why (emgr's SSRF
// guard unconditionally blocks RFC1918 addresses, so it must reach the
// origin over a shared-namespace loopback instead of the normal docker
// network hostname imgproxy uses).
const ORIGIN_SOURCE_BASE_URL = __ENV.ORIGIN_SOURCE_BASE_URL || 'http://origin:80';

const VUS = parseInt(__ENV.VUS || '10', 10);
const DURATION = __ENV.DURATION || '15s';

// fixture:widthxheight, e.g. "photo_4k.jpg:3840x2160" -- the driver needs
// each fixture's own resolution so it never asks for an upscale (emgr
// refuses to upscale outright -- src/models/params.rs's `enlarge` field
// has no query parameter to opt in yet -- and comparing an upscale imgproxy
// happily performs against an emgr 400 would not be a fair or even
// meaningful comparison).
const FIXTURES = (__ENV.FIXTURES || [
  'photo_4k.jpg:3840x2160',
  'photo_1080p.jpg:1920x1080',
  'photo_800x600.jpg:800x600',
  'alpha_1024.png:1024x1024',
  'flat_1024.png:1024x1024',
].join(',')).split(',').map((entry) => {
  const [name, dims] = entry.split(':');
  const [w, h] = dims.split('x').map(Number);
  return { name, w, h };
});

const SIZES = (__ENV.SIZES || '300x300,640x480,1200x800').split(',').map((s) => {
  const [w, h] = s.split('x').map(Number);
  return { w, h };
});

const FORMATS = (__ENV.FORMATS || 'jpg,png,webp').split(',');

// Every (fixture, size, format) combo that doesn't require upscaling
// either dimension -- this is the actual request matrix both engines see.
const COMBOS = [];
for (const fixture of FIXTURES) {
  for (const size of SIZES) {
    if (size.w > fixture.w || size.h > fixture.h) continue; // would upscale
    for (const format of FORMATS) {
      COMBOS.push({ fixture, size, format });
    }
  }
}

if (COMBOS.length === 0) {
  throw new Error('COMBOS is empty -- check FIXTURES/SIZES/FORMATS env vars');
}

// ---------------------------------------------------------------------
// URL builders -- the part that changes when emgr's URL API changes.
// ---------------------------------------------------------------------

// imgproxy's "plain" source mode takes the source URL almost verbatim;
// only %, ?, and @ need percent-escaping (they're syntactically meaningful
// in imgproxy's own path grammar). A full encodeURIComponent would also
// escape ':' and '/', corrupting the URL.
function imgproxyPlainEncode(url) {
  return url.replace(/%/g, '%25').replace(/\?/g, '%3F').replace(/@/g, '%40');
}

const urlBuilders = {
  // Current emgr API: query params on GET /api/images/resize, see
  // openapi.yaml. 301-redirects to CDN_BASE_URL/api/images/files/{key};
  // k6 follows redirects by default, so `res.timings.duration` below
  // covers the full round trip (initial resize/lookup + the storage
  // fetch), matching how a real client experiences it.
  emgr(sourceUrl, w, h, format) {
    const qs = `url=${encodeURIComponent(sourceUrl)}&width=${w}&height=${h}&format=${format}`;
    return `${ENGINE_BASE_URL}/api/images/resize?${qs}`;
  },

  // imgproxy: /{signature}/{options}/plain/{source}. "insecure" as the
  // signature segment because IMGPROXY_KEY/IMGPROXY_SALT are unset in
  // compose.yaml (imgproxy's documented way to disable signing).
  // rt:fit matches emgr's own only resize behavior (no crop mode exposed
  // in emgr's current API), so both engines are asked to do the same
  // "fit within WxH, preserve aspect ratio" operation.
  imgproxy(sourceUrl, w, h, format) {
    const options = `w:${w}/h:${h}/rt:fit/f:${format}`;
    return `${ENGINE_BASE_URL}/insecure/${options}/plain/${imgproxyPlainEncode(sourceUrl)}`;
  },
};

// ---------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------

const outcome2xx = new Counter('outcome_2xx');
const outcomeNon2xx = new Counter('outcome_non2xx');
const outcomeTimeout = new Counter('outcome_timeout');
const outcomeConnError = new Counter('outcome_conn_error');
// Response byte size, per (fixture, size, format) -- this is the "output
// size comparison at comparable quality settings" data. Read from
// Content-Length rather than the body so discardResponseBodies can stay
// true (large 4K derivatives would otherwise bloat k6's memory under
// concurrency).
const responseBytes = new Trend('response_bytes', false);

// ---------------------------------------------------------------------
// k6 options
// ---------------------------------------------------------------------

export const options = {
  discardResponseBodies: true,
  scenarios: {
    load: {
      executor: 'constant-vus',
      vus: VUS,
      duration: DURATION,
    },
  },
  summaryTrendStats: ['avg', 'min', 'med', 'p(90)', 'p(99)', 'p(99.9)', 'max'],
};

// ---------------------------------------------------------------------
// Request logic
// ---------------------------------------------------------------------

let iterCounter = 0;

export default function () {
  const combo = COMBOS[iterCounter % COMBOS.length];
  iterCounter += 1;

  // Cold cache: a unique `?variant=` query string per request forces a
  // distinct cache key (both engines fold the full source URL into their
  // cache key) without changing the actual image bytes served by nginx
  // (nginx ignores query strings for static files, see origin/nginx.conf)
  // or the requested transform -- so decode/resize/encode cost is
  // identical to the warm case, only the cache lookup differs.
  //
  // Warm cache: a fixed variant means every iteration requests the exact
  // same derivative. See README.md's "warm cache" section for why this
  // scenario is NOT apples-to-apples between the two engines (emgr can
  // short-circuit to a redirect once the derivative exists; imgproxy has
  // no equivalent result cache and reprocesses every request).
  const variant = SCENARIO === 'cold' ? `${__VU}-${__ITER}-${Date.now()}` : 'fixed';

  const sourceUrl = `${ORIGIN_SOURCE_BASE_URL}/corpus/${combo.fixture.name}?variant=${variant}`;
  const url = urlBuilders[ENGINE](sourceUrl, combo.size.w, combo.size.h, combo.format);

  const res = http.get(url, {
    tags: {
      fixture: combo.fixture.name,
      size: `${combo.size.w}x${combo.size.h}`,
      format: combo.format,
    },
    timeout: '30s',
  });

  if (res.status === 0) {
    // k6 transport-level failure: no HTTP response at all. Split
    // timeout from other connection errors via the error string k6
    // attaches -- there's no stable numeric error_code contract across
    // k6 versions worth hardcoding here.
    if ((res.error || '').toLowerCase().includes('timeout')) {
      outcomeTimeout.add(1);
    } else {
      outcomeConnError.add(1);
    }
  } else if (res.status >= 200 && res.status < 300) {
    outcome2xx.add(1);
    const len = res.headers['Content-Length'];
    if (len) {
      responseBytes.add(Number(len), {
        fixture: combo.fixture.name,
        size: `${combo.size.w}x${combo.size.h}`,
        format: combo.format,
      });
    }
  } else {
    outcomeNon2xx.add(1);
  }

  check(res, {
    'got a response': (r) => r.status !== 0,
  });
}

// ---------------------------------------------------------------------
// Summary: write a compact, machine-readable JSON report alongside k6's
// own stdout text summary, tagged with the run's own configuration so
// results/*.json files are self-describing.
// ---------------------------------------------------------------------

export function handleSummary(data) {
  const report = {
    engine: ENGINE,
    scenario: SCENARIO,
    vus: VUS,
    duration: DURATION,
    combos: COMBOS.length,
    metrics: {
      http_req_duration: data.metrics.http_req_duration
        ? data.metrics.http_req_duration.values
        : null,
      response_bytes: data.metrics.response_bytes
        ? data.metrics.response_bytes.values
        : null,
      outcome_2xx: data.metrics.outcome_2xx ? data.metrics.outcome_2xx.values.count : 0,
      outcome_non2xx: data.metrics.outcome_non2xx ? data.metrics.outcome_non2xx.values.count : 0,
      outcome_timeout: data.metrics.outcome_timeout ? data.metrics.outcome_timeout.values.count : 0,
      outcome_conn_error: data.metrics.outcome_conn_error
        ? data.metrics.outcome_conn_error.values.count
        : 0,
      iterations: data.metrics.iterations ? data.metrics.iterations.values.count : 0,
      throughput_rps: data.metrics.http_reqs ? data.metrics.http_reqs.values.rate : null,
    },
  };

  return {
    stdout: JSON.stringify(report, null, 2) + '\n',
    [`/results/${ENGINE}-${SCENARIO}-vus${VUS}.json`]: JSON.stringify(report, null, 2),
  };
}
