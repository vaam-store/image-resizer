// Load driver for the emgr-vs-imgproxy benchmark. Runs inside the
// `grafana/k6` container, on the same `bench` docker network as the
// services under test -- see ../compose.yaml and ../README.md.
//
// The URL shape for each engine lives entirely in the `urlBuilders` map
// below, keyed by ENGINE. Both engines now use an imgproxy-style signed
// path; emgr's rewrite (#53/#27) has landed, so the old query-parameter
// endpoint no longer exists.
import http from 'k6/http';
import { Counter, Trend } from 'k6/metrics';
import { check } from 'k6';
import encoding from 'k6/encoding';

// ---------------------------------------------------------------------
// Configuration (all via env vars, set by ../driver/run.sh per scenario)
// ---------------------------------------------------------------------

const ENGINE = __ENV.ENGINE || 'emgr'; // 'emgr' | 'emgr_s3' | 'imgproxy'
const SCENARIO = __ENV.SCENARIO || 'cold'; // 'cold' | 'warm'
const ENGINE_BASE_URL = __ENV.ENGINE_BASE_URL || 'http://emgr:3000';
// Where the *proxy itself* fetches source images from -- NOT necessarily
// where k6 fetches the proxy from. The same for every engine (#57): all
// three reach `origin` over the normal `bench` bridge network by service
// name. emgr/emgr_s3 are authorized to do so via
// ALLOWED_SOURCES=http://origin:80/ in compose.yaml, which lifts the
// SSRF guard's private-range block for that one named host - see that
// file's header comment and src/services/image/source_guard.rs.
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

// base64url, no padding — emgr's source encoding. Chosen over its
// `plain/` form because the source URL contains slashes, and a base64url
// segment never does, so there is no ambiguity about what got signed.
function base64UrlEncode(s) {
  return encoding
    .b64encode(s, 'rawurl');
}

const urlBuilders = {
  // emgr's signed-path API (#53/#27), which replaced the old
  // /api/images/resize query endpoint. Grammar:
  //   /{signature}/{processing_options}/{base64url source}.{extension}
  //
  // "unsigned" as the signature segment, mirroring imgproxy's "insecure"
  // below: ALLOW_UNSIGNED_REQUESTS=true is set in compose.yaml so both
  // engines skip signature verification and the comparison stays about
  // image processing rather than HMAC throughput.
  //
  // rs:fit:{w}:{h} matches the `rt:fit` asked of imgproxy — both engines
  // are told to fit within WxH preserving aspect ratio. el:0 forbids
  // upscaling on emgr's side; the fixture-resolution logic above already
  // guarantees we never request one.
  //
  // Still 301-redirects to CDN_BASE_URL/...; k6 follows redirects by
  // default, so res.timings.duration covers the full round trip (resize
  // plus the storage fetch), which is how a real client experiences it.
  emgr(sourceUrl, w, h, format) {
    const options = `rs:fit:${w}:${h}/el:0`;
    return `${ENGINE_BASE_URL}/unsigned/${options}/${base64UrlEncode(sourceUrl)}.${format}`;
  },

  // emgr on the S3/MinIO storage backend -- identical URL grammar to plain
  // `emgr` above (the URL API doesn't change with the storage backend,
  // only what CDN_BASE_URL the 301 Location header points at -- see
  // ../compose.yaml's `emgr_s3` service and its "THE POINT OF THE
  // THREE-WAY SPLIT" header comment). ENGINE_BASE_URL/ORIGIN_SOURCE_BASE_URL
  // still differ per engine via ../driver/run.sh's engine_base_url()/
  // origin_source_base_url(), so this can't just be an alias assignment to
  // `emgr` -- it needs its own entry even though the body is the same.
  emgr_s3(sourceUrl, w, h, format) {
    const options = `rs:fit:${w}:${h}/el:0`;
    return `${ENGINE_BASE_URL}/unsigned/${options}/${base64UrlEncode(sourceUrl)}.${format}`;
  },

  // imgproxy: /{signature}/{options}/plain/{source}. "insecure" as the
  // signature segment because IMGPROXY_KEY/IMGPROXY_SALT are unset in
  // compose.yaml (imgproxy's documented way to disable signing).
  // rt:fit matches emgr's `rs:fit:{w}:{h}` above, so both engines are
  // asked to do the same "fit within WxH, preserve aspect ratio"
  // operation and produce the same output dimensions for a given source.
  //
  // This was `rt:fill` for a while: before #59, emgr parsed the `rs`
  // segment's type token but silently ignored it and always cropped to
  // exact WxH (GH #59) - asking imgproxy for `fit` got 800x450 for a 16:9
  // source while emgr returned 800x600, 33% more pixels of work and a
  // differently-composed image, which made the comparison invalid. `fill`
  // was a stopgap to match what emgr actually did. #59 fixed emgr to
  // honour `fit`/`fill`/`force`/`auto` for real, so this reverts to the
  // originally-intended `rt:fit` now that both engines actually perform
  // the same operation.
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
      // NOTE: `http_reqs.rate` counts every HTTP request, and the engines do
      // NOT issue the same number of them per delivered image. emgr answers
      // with a 301 to storage and k6 follows it, so one delivered image costs
      // emgr ~2 requests; imgproxy streams the bytes from the same request, so
      // it costs 1. Comparing this number between engines therefore flatters
      // emgr by roughly 2x. Use `images_per_second` below for the throughput
      // comparison; this stays for continuity with earlier runs and for
      // per-engine trend tracking, where the request-count ratio is constant.
      throughput_rps: data.metrics.http_reqs ? data.metrics.http_reqs.values.rate : null,
      http_reqs_per_iteration:
        data.metrics.http_reqs && data.metrics.iterations && data.metrics.iterations.values.count
          ? data.metrics.http_reqs.values.count / data.metrics.iterations.values.count
          : null,
      // The honest cross-engine throughput number: completed image deliveries
      // per second, independent of how many HTTP round trips each engine takes
      // to deliver one.
      images_per_second: data.metrics.iterations ? data.metrics.iterations.values.rate : null,
      // The honest cross-engine latency number: wall-clock for one complete
      // image delivery, redirect hops included. `http_req_duration` is
      // per-request, so for emgr it blends near-instant 301s (min ~0.1ms) with
      // real transforms into one bimodal distribution - which drags its median
      // down and makes a p50 "at parity" with imgproxy an artefact of the
      // mixture rather than a statement about processing speed.
      iteration_duration: data.metrics.iteration_duration
        ? data.metrics.iteration_duration.values
        : null,
    },
  };

  return {
    stdout: JSON.stringify(report, null, 2) + '\n',
    [`/results/${ENGINE}-${SCENARIO}-vus${VUS}.json`]: JSON.stringify(report, null, 2),
  };
}
