#!/usr/bin/env node
// Drives the real FreeBank phone page against this app's desktop side (the page_host test) and a
// real FreeBank node, in headless WebKit (iPhone), Chromium (Pixel 7) and Firefox (a phone-sized
// window). scripts/phone-real-node.sh builds and starts everything and runs this; the env it
// passes is read below. Screenshots go to $FB_SHOTS/<browser>/.
//
// First, once: a stranger's channel (a bare WebSocket that joins the room and says a bad hello) is
// closed about 30 s after its first frame (P5).
// Per browser: pair while a second phone asks with the same QR code under the same name, allowing
// the request whose comparison code this phone shows (P1); balance, history and block height against
// the node; a receive address the
// node's wallet owns (validateaddress) and its QR code; phone sends turned on with the wallet
// passphrase and a send within the limit (a receipt the node knows, or with an empty wallet a
// clear "Not sent"); phone sends off, so a send waits for the desktop ("locked") until it expires;
// a send over the limit confirmed on the desktop with a wrong passphrase, none, then the right
// one; revoke. The wallet must be locked again after every step that unlocked it.
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, renameSync, rmSync, unlinkSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const need = (k) => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} is not set (run scripts/phone-real-node.sh)`);
  return v;
};
const PHONE = need('FB_PHONE_DIR'); // freebank-distribution/phone: Playwright and jsQR live there
const ORIGIN = need('FB_ORIGIN'); // the relay, serving the page
const CTL = need('FB_CTL_DIR'); // page_host's control folder
const SHOTS = need('FB_SHOTS');
const CLI = need('FB_CLI');
const DATADIR = need('FB_NODE_DATADIR');
const RPCPORT = need('FB_NODE_RPCPORT');
const HELD_SECS = Number(process.env.FB_HELD_SECS || 20);
const SEND_ECX = process.env.FB_SEND_ECX || '0.01';
const OVER_ECX = process.env.FB_OVER_ECX || '0.5';
const ENCRYPTED = process.env.FB_ENCRYPTED === '1';

const NOT_ENOUGH = 'Not enough sECX in your desktop wallet for this payment and its fee.';
const FEE_ROOM = 0.001;

process.env.PLAYWRIGHT_BROWSERS_PATH = '0';
const wkLibs = join(PHONE, 'node_modules/.wk-libs/usr/lib/x86_64-linux-gnu');
if (existsSync(wkLibs)) process.env.LD_LIBRARY_PATH = [wkLibs, process.env.LD_LIBRARY_PATH].filter(Boolean).join(':');
const { webkit, chromium, firefox, devices } = await import(pathToFileURL(join(PHONE, 'node_modules/playwright/index.mjs')).href);
const jsQR = createRequire(join(PHONE, 'package.json'))('jsqr');

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---------- the node (freebank-cli with the node's cookie; nothing secret is printed) ----------
// A node that stops answering fails the run in seconds rather than holding it for the CLI's 15 min.
const cli = (...args) =>
  execFileSync(CLI, [`-datadir=${DATADIR}`, `-rpcport=${RPCPORT}`, '-rpcclienttimeout=20', ...args], {
    encoding: 'utf8',
    timeout: 30_000,
  }).trim();
const answers = () => {
  try {
    execFileSync(CLI, [`-datadir=${DATADIR}`, `-rpcport=${RPCPORT}`, '-rpcclienttimeout=5', 'getblockcount'], { timeout: 10_000 });
    return true;
  } catch {
    return false;
  }
};
const cliJson = (...args) => JSON.parse(cli(...args));
const balance = () => Number(cli('getbalance'));
const locked = () => {
  const w = cliJson('getwalletinfo');
  return !('unlocked_until' in w) || w.unlocked_until === 0; // an unencrypted wallet counts as "not left unlocked"
};

// ---------- page_host's control folder ----------
let seq = 0;
async function ctl(line, timeoutMs = 60_000) {
  const n = ++seq;
  try {
    unlinkSync(join(CTL, 'ack'));
  } catch {}
  writeFileSync(join(CTL, '.cmd.tmp'), `${n} ${line}\n`);
  renameSync(join(CTL, '.cmd.tmp'), join(CTL, 'cmd'));
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutMs) {
    try {
      const a = JSON.parse(readFileSync(join(CTL, 'ack'), 'utf8'));
      if (String(a.seq) === String(n)) return a;
    } catch {}
    await sleep(100);
  }
  throw new Error(`the desktop didn't answer "${line.split(' ')[0]}"`);
}
const state = () => JSON.parse(readFileSync(join(CTL, 'state.json'), 'utf8'));
async function until(what, fn, timeoutMs = 15_000) {
  const t0 = Date.now();
  for (;;) {
    const v = fn();
    if (v) return v;
    if (Date.now() - t0 > timeoutMs) throw new Error(`timed out waiting for ${what}`);
    await sleep(200);
  }
}

// ---------- the page's own formatting ----------
function fmtEcx(n) {
  const neg = n < 0;
  let s = Math.abs(n).toFixed(8).replace(/0+$/, '');
  const [i, f = ''] = s.split('.');
  s = i.replace(/\B(?=(\d{3})+(?!\d))/g, ',') + '.' + f.padEnd(2, '0');
  return (neg ? '-' : '') + s;
}

async function qrFromPage(page) {
  // Read the QR <svg> the page drew, rasterise its modules here, and decode it with jsQR.
  const { d, size } = await page.evaluate(() => {
    const svg = document.querySelector('[data-testid=qr]');
    return { d: svg.querySelector('path').getAttribute('d'), size: Number(svg.getAttribute('viewBox').split(' ')[2]) };
  });
  const scale = 6, w = size * scale;
  const px = new Uint8ClampedArray(w * w * 4).fill(255);
  for (const [, x, y] of d.matchAll(/M(\d+) (\d+)/g))
    for (let dy = 0; dy < scale; dy++)
      for (let dx = 0; dx < scale; dx++) {
        const o = ((+y * scale + dy) * w + (+x * scale + dx)) * 4;
        px[o] = px[o + 1] = px[o + 2] = 0;
      }
  return jsQR(px, w, w)?.data ?? null;
}

const ENGINES = {
  webkit: { type: webkit, ctx: { ...devices['iPhone 13'], viewport: { width: 390, height: 844 } } },
  chromium: { type: chromium, ctx: { ...devices['Pixel 7'] } },
  firefox: {
    type: firefox,
    ctx: { viewport: { width: 412, height: 915 }, hasTouch: true, deviceScaleFactor: 2,
      userAgent: 'Mozilla/5.0 (Android 14; Mobile; rv:128.0) Gecko/128.0 Firefox/128.0' },
  },
};

async function runEngine(name) {
  const { type, ctx } = ENGINES[name];
  const results = [];
  const check = (what, ok, extra = '') => {
    results.push({ what, ok: !!ok });
    console.log(`  [${name}] ${ok ? 'ok  ' : 'FAIL'} ${what}${extra ? ' — ' + extra : ''}`);
  };
  const shots = join(SHOTS, name);
  rmSync(shots, { recursive: true, force: true });
  mkdirSync(shots, { recursive: true });
  let n = 0;
  let inShot = false;
  const shot = async (page, label) => {
    inShot = true; // Playwright's screenshot style is refused by the page's CSP; not the page's fault
    try {
      await page.screenshot({ path: join(shots, `${String(++n).padStart(2, '0')}-${label}.png`), caret: 'initial' });
    } finally {
      inShot = false;
    }
  };

  const browser = await type.launch();
  const problems = [];
  try {
    const context = await browser.newContext({ ...ctx, colorScheme: 'dark' });
    const page = await context.newPage();
    page.on('pageerror', (e) => problems.push('pageerror: ' + e.message));
    page.on('console', (m) => {
      if (inShot && /Refused to apply a stylesheet|Content Security Policy/.test(m.text())) return;
      if (m.type() === 'error' || /Content Security Policy|CSP/i.test(m.text())) problems.push('console: ' + m.text());
    });
    const home = async () => {
      if (!(await page.getByTestId('home').isVisible())) await page.getByRole('button', { name: 'Back' }).click();
      await page.getByTestId('home').waitFor();
    };
    // Fill the send form up to its last button. Returns what the phone said was left of today's
    // limit.
    const prepare = async (to, ecx) => {
      await home();
      await page.getByRole('button', { name: 'Send', exact: true }).click();
      await page.getByTestId('limit').getByText('ECX').waitFor({ timeout: 15_000 });
      const left = (await page.getByTestId('limit').locator('strong').textContent()).trim();
      await page.getByLabel('Pay to address').fill(to);
      await page.getByLabel('Amount').fill(ecx);
      await page.getByRole('button', { name: 'Review' }).click();
      return left;
    };
    // Press it, and wait until the page shows a result or the held screen.
    const submit = async (ecx) => {
      await page.getByRole('button', { name: `Send ${fmtEcx(Number(ecx))} ECX` }).click();
      await page.locator('[data-testid=receipt], [data-testid=send-error], [data-testid=held]').first().waitFor({ timeout: 30_000 });
    };
    const send = async (to, ecx) => {
      const left = await prepare(to, ecx);
      await submit(ecx);
      return left;
    };
    const leaveSend = async () => {
      if (await page.getByRole('button', { name: 'Done' }).isVisible()) await page.getByRole('button', { name: 'Done' }).click();
      else if (await page.getByRole('button', { name: 'Back to the form' }).isVisible()) await page.getByRole('button', { name: 'Back to the form' }).click();
      await home();
    };
    // The result of a send that either goes out (the wallet has coins) or fails clearly (empty).
    const outcome = async (what, ecx, before) => {
      const paid = before >= Number(ecx) + FEE_ROOM;
      if (paid) {
        await page.getByTestId('receipt').waitFor({ timeout: 30_000 });
        const txid = (await page.getByTestId('receipt-txid').textContent()).trim();
        let known = false;
        try {
          known = cliJson('gettransaction', txid).txid === txid;
        } catch {}
        check(`${what}: a receipt the node's wallet knows`, known, txid);
      } else {
        await page.getByTestId('send-error').waitFor({ timeout: 30_000 });
        const msg = (await page.getByTestId('send-error').locator('.error').textContent()).trim();
        check(`${what}: with ${fmtEcx(before)} ECX it fails with a clear message`, msg === NOT_ENOUGH, msg);
      }
      return paid;
    };

    // 1. Pair. Someone else who saw the QR code asks first, under the same name (P1).
    const pair = await ctl('pair');
    const other = await browser.newContext({ ...ctx, colorScheme: 'dark' });
    const intruder = await other.newPage();
    const askToPair = async (p) => {
      await p.goto(pair.ok.url);
      await p.getByLabel('Name for this phone').fill(`Real ${name}`);
      await p.getByRole('button', { name: 'Pair' }).click();
      await p.getByTestId('pair-code').locator('strong').waitFor({ timeout: 20_000 });
      return (await p.getByTestId('pair-code').locator('strong').textContent()).trim();
    };
    const theirs = await askToPair(intruder);
    const code = await askToPair(page);
    check('pairing link opened, fragment stripped', !page.url().includes('#pair'), page.url());
    check('the phone shows a comparison code', /^\d{3} \d{3}$/.test(code), code);
    await shot(page, 'pair-code');
    const asks = await until('both requests on the desktop', () => (state().asks.length === 2 ? state().asks : null));
    check('the desktop shows each request with its own code', asks.some((a) => a.code === code) &&
      asks.some((a) => a.code === theirs) && code !== theirs && asks.every((a) => a.name === `Real ${name}`),
      `this phone ${code}, the other ${theirs}`);
    const ours = asks.find((a) => a.code === code);
    const allowed = await ctl(`allow ${ours.id}`);
    check('allowed by its code', !!allowed.ok, allowed.err ?? '');
    await page.getByTestId('home').waitFor({ timeout: 20_000 });
    await intruder.getByText('Not paired').waitFor({ timeout: 20_000 });
    await other.close();
    check('this phone is paired and the other refused', state().devices.length === 1 && state().asks.length === 0);
    await page.getByTestId('status').getByText('Connected').waitFor({ timeout: 20_000 });
    check('paired and connected', await until('the phone online on the desktop',
      () => state().devices.some((d) => d.name === `Real ${name}` && d.online)).catch(() => false));

    // 2. Balance, status, history, against the node.
    const bal = balance();
    await page.getByTestId('balance').getByText(fmtEcx(bal)).waitFor({ timeout: 20_000 });
    check('balance matches the node', true, `${fmtEcx(bal)} ECX`);
    const meta = await page.locator('.meta').textContent({ timeout: 10_000 });
    const shown = Number(meta.replace(/[^\d]/g, '').trim() || -1);
    const height = Number(cli('getblockcount'));
    check('block height matches the node', Math.abs(shown - height) <= 2, `page ${shown}, node ${height}`);
    check('node reported synced', !meta.includes('still syncing'), meta.trim());
    const txs = cliJson('listtransactions', '*', '10');
    if (txs.length === 0) {
      check('history: no payments yet', await page.getByText('No payments yet.').isVisible());
    } else {
      const rows = await page.getByTestId('txs').locator('li').count();
      await page.getByTestId('txs').locator('li button.tx').first().click();
      const first = (await page.getByTestId('txid').first().textContent()).trim();
      check('history: the newest first', rows === txs.length && first === txs[txs.length - 1].txid, `${rows} rows, first ${first}`);
      await page.getByTestId('txs').locator('li button.tx').first().click();
    }
    await shot(page, 'home');

    // 3. Receive: an address of this node's wallet, and its QR code.
    await page.getByRole('button', { name: 'Receive', exact: true }).click();
    await page.getByTestId('qr').waitFor({ timeout: 15_000 });
    const address = (await page.getByTestId('address').textContent()).trim();
    // This freebankd's validateaddress checks the address; getaddressinfo says whose it is.
    const v = cliJson('validateaddress', address);
    let mine = v.ismine;
    try {
      mine = cliJson('getaddressinfo', address).ismine;
    } catch {}
    check('receive address is valid and the node\'s wallet owns it', v.isvalid === true && mine === true, address);
    check('receive QR decodes to it', (await qrFromPage(page)) === address);
    await shot(page, 'receive');
    await home();
    const to = process.env.FB_TO || address; // paying ourselves keeps the coins (less the fee)

    // 4. Phone sends on, and a send within the limit. The send is pressed just before the node's
    // relock of the passphrase check (1 s) fires: an unlock then would deadlock freebankd, so the
    // desktop must wait for the relock.
    let before = balance();
    let left = await prepare(to, SEND_ECX);
    check('status: the whole daily limit is left', left === '0.10 ECX', left);
    if (ENCRYPTED) {
      const w = await ctl('wallet');
      check('the wallet is encrypted and locked', w.ok?.encrypted === true && w.ok?.locked === true, JSON.stringify(w.ok ?? w.err));
      const on = await ctl('send-on');
      const t = Date.now();
      check('phone sends turned on with the passphrase', on.ok && state().phone_send, on.err ?? '');
      check('wallet locked again after the check', locked());
      await sleep(Math.max(0, 850 - (Date.now() - t)));
    }
    const t4 = Date.now();
    await submit(SEND_ECX);
    const took = ((Date.now() - t4) / 1000).toFixed(1);
    await shot(page, 'send-within-limit');
    const paid = await outcome(`send ${SEND_ECX} within the limit (${took} s)`, SEND_ECX, before);
    check('the node still answers (no deadlock at the relock)', answers());
    check('wallet locked again after the send', locked());
    await leaveSend();

    // 5. Phone sends off: a send within the limit waits for the desktop, then expires.
    if (ENCRYPTED) {
      const off = await ctl('send-off');
      check('phone sends turned off', off.ok && !state().phone_send);
      const txCount = cliJson('listtransactions', '*', '1000').length;
      left = await send(to, SEND_ECX);
      const want = `${fmtEcx(paid ? 0.1 - Number(SEND_ECX) : 0.1)} ECX`;
      check(paid ? 'status: the send came off the limit' : 'status: a failed send gave its allowance back', left === want, left);
      await page.getByTestId('held').waitFor({ timeout: 15_000 });
      const h = await until('the held send on the desktop', () => state().held[0]);
      check('held on the desktop because the wallet is locked', h.why === 'locked', JSON.stringify({ why: h.why, amount: h.amount }));
      check('the phone shows it waiting for the desktop', await page.getByText('Waiting for your desktop').isVisible());
      await shot(page, 'held-locked');
      const t0 = Date.now();
      await page.getByTestId('send-error').waitFor({ timeout: (HELD_SECS + 15) * 1000 });
      const msg = (await page.getByTestId('send-error').locator('.error').textContent()).trim();
      const secs = Math.round((Date.now() - t0) / 1000);
      check('after its time the phone hears it expired', /not confirmed on the desktop within/.test(msg), `${msg} (${secs} s)`);
      check('and the desktop no longer shows it', await until('the desktop to drop it', () => state().held.length === 0).catch(() => false));
      check('nothing was sent', cliJson('listtransactions', '*', '1000').length === txCount);
      await shot(page, 'held-expired');
      await leaveSend();
    }

    // 6. Over the limit: held, then confirmed on the desktop (with the passphrase if encrypted).
    before = balance();
    await send(to, OVER_ECX);
    await page.getByTestId('held').waitFor({ timeout: 15_000 });
    const h2 = await until('the held send on the desktop', () => state().held[0]);
    check('over the limit it is held for the desktop', h2.why === 'limit', `why ${h2.why}`);
    await shot(page, 'held-over-limit');
    if (ENCRYPTED) {
      const wrong = await ctl(`confirm ${h2.confirm} wrong`);
      check('a wrong passphrase is refused and the send keeps waiting',
        /isn't the wallet's passphrase/.test(wrong.err ?? '') && state().held.length === 1 && (await page.getByTestId('held').isVisible()),
        wrong.err ?? JSON.stringify(wrong.ok));
      const none = await ctl(`confirm ${h2.confirm} none`);
      check('no passphrase: the desktop is asked for one', none.ok?.need_passphrase === true && state().held.length === 1);
      check('wallet still locked', locked());
      const good = await ctl(`confirm ${h2.confirm} pass`);
      check('confirmed with the passphrase', before >= Number(OVER_ECX) + FEE_ROOM ? !!good.ok?.txid : good.err === NOT_ENOUGH, JSON.stringify(good.ok ?? good.err));
    } else {
      const good = await ctl(`confirm ${h2.confirm} none`);
      check('confirmed on the desktop', before >= Number(OVER_ECX) + FEE_ROOM ? !!good.ok?.txid : good.err === NOT_ENOUGH, JSON.stringify(good.ok ?? good.err));
    }
    await outcome(`held send of ${OVER_ECX}`, OVER_ECX, before);
    check('wallet locked again after the desktop\'s send', locked());
    check('the desktop has nothing waiting', await until('the desktop to clear it', () => state().held.length === 0).catch(() => false));
    await shot(page, 'held-answered');
    await leaveSend();

    // 7. Revoke.
    await ctl('revoke');
    await page.getByText('This phone was removed').waitFor({ timeout: 20_000 });
    check('revoked phone is told so', await until('the desktop to forget it', () => state().devices.length === 0).catch(() => false));
    await shot(page, 'revoked');

    const netNoise = problems.filter((m) => /WebSocket connection to .* failed/.test(m));
    const real = problems.filter((m) => !netNoise.includes(m));
    check('no page errors or CSP violations', real.length === 0, real.join(' | '));
    await context.close();
  } catch (e) {
    check('flow completed', false, e.message.split('\n')[0]);
    if (problems.length) console.log('  problems:', problems.join(' | '));
    // Leave the desktop clean for the next browser.
    try {
      await ctl('revoke');
    } catch {}
  } finally {
    await browser.close();
  }
  return results;
}

// P5: a stranger's channel is closed about 30 s after its first frame. A bare WebSocket (no Origin
// header, which the relay allows, as for the desktop) joins the room and says a hello no desktop
// knows; the desktop answers "denied", then asks the relay to drop the channel.
async function strangerIsDropped() {
  const results = [];
  const check = (what, ok, extra = '') => {
    results.push({ what, ok: !!ok });
    console.log(`  [stranger] ${ok ? 'ok  ' : 'FAIL'} ${what}${extra ? ' — ' + extra : ''}`);
  };
  const { default: WebSocket } = await import(pathToFileURL(join(PHONE, 'node_modules/ws/wrapper.mjs')).href);
  const pair = await ctl('pair'); // an open pairing brings the desktop online
  const link = JSON.parse(Buffer.from(pair.ok.url.split('#pair=')[1], 'base64url').toString());
  const ws = new WebSocket(link.relay);
  const frames = [];
  const closed = new Promise((res) => ws.on('close', (code) => res({ code, at: Date.now() })));
  ws.on('message', (m) => frames.push(JSON.parse(m.toString())));
  await new Promise((res, rej) => {
    ws.on('open', res);
    ws.on('error', rej);
  });
  ws.send(JSON.stringify({ t: 'join', room: link.room }));
  await until('joined', () => frames.some((f) => f.t === 'joined'), 10_000);
  const t0 = Date.now();
  const bogus = Buffer.alloc(65, 4).toString('base64url');
  ws.send(JSON.stringify({ t: 'hello', p: bogus, e: bogus }));
  await until('the answer', () => frames.some((f) => f.t === 'denied'), 10_000).catch(() => {});
  check('an unknown hello is denied', frames.some((f) => f.t === 'denied'), JSON.stringify(frames.at(-1)));
  const end = await Promise.race([closed, sleep(60_000).then(() => null)]);
  const secs = end ? (end.at - t0) / 1000 : null;
  check('and its channel is closed about 30 s later', secs !== null && secs >= 28.5 && secs <= 36, secs === null ? 'still open after 60 s' : `${secs.toFixed(1)} s`);
  if (!end) ws.terminate();
  return results;
}

const args = process.argv.slice(2);
const wanted = args.length ? args : Object.keys(ENGINES);
console.log(`node: height ${cli('getblockcount')}, balance ${fmtEcx(balance())} ECX, wallet ${ENCRYPTED ? 'encrypted' : 'not encrypted'}`);
const summary = {};
if (!process.env.FB_SKIP_STRANGER) {
  console.log('\n== stranger');
  try {
    const r = await strangerIsDropped();
    summary.stranger = `${r.filter((x) => x.ok).length}/${r.length} passed`;
  } catch (e) {
    summary.stranger = 'could not run: ' + e.message.split('\n')[0];
    console.log('  ' + summary.stranger);
  }
}
for (const name of wanted) {
  console.log(`\n== ${name}`);
  try {
    const r = await runEngine(name);
    summary[name] = `${r.filter((x) => x.ok).length}/${r.length} passed`;
  } catch (e) {
    summary[name] = 'could not run: ' + e.message.split('\n')[0];
    console.log('  ' + summary[name]);
  }
}
console.log('\nsummary', summary);
process.exitCode = Object.values(summary).every((s) => /^(\d+)\/\1 passed$/.test(s)) ? 0 : 1;
