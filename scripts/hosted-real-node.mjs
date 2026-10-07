#!/usr/bin/env node
// Hosted wallets (v0.2.8) against two real desktops and real nodes: scripts/hosted-real-node.sh builds and starts
// everything and runs this. Moving home is to fresh words (PROTOCOL.md, "Moving home"): desktop B is set up as new,
// and the house moves the notes and the ECX to the address desktop B gives. Two phones in headless Chromium (Pixel 7), each with a CDP WebAuthn virtual authenticator,
// so the desktop checks real passkey assertions. Steps stop at the first failure; each result is printed.
import { execFileSync, execSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, readdirSync, renameSync, unlinkSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const need = (k) => {
  const v = process.env[k];
  if (!v) throw new Error(`${k} is not set (run scripts/hosted-real-node.sh)`);
  return v;
};
const PHONE = need('FB_PHONE_DIR');
const ORIGIN = need('FB_ORIGIN');
const CTL_A = need('FB_CTL_A');
const CTL_B = need('FB_CTL_B');
const SHOTS = need('FB_SHOTS');
// Node A has the house's wallet and, once made, hosted ones open: name the main wallet.
const A_CLI = [...need('FB_A_CLI').split(' '), '-rpcwallet=wallet.dat'];
const A_PASS = need('FB_A_PASS_FILE');
const A_DATADIR = need('FB_A_DATADIR');
const A_RESTART = need('FB_A_RESTART_CMD');
const B_CLI = need('FB_B_CLI').split(' ');
const A_CLI_RAW = need('FB_A_CLI').split(' ');
const BMM = need('FB_BMM_CMD');
const WORK = need('FB_WORK');

process.env.PLAYWRIGHT_BROWSERS_PATH = '0';
const { chromium, devices } = await import(pathToFileURL(join(PHONE, 'node_modules/playwright/index.mjs')).href);
const jsQR = createRequire(join(PHONE, 'package.json'))('jsqr');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const run = (argv, ...args) => execFileSync(argv[0], [...argv.slice(1), '-rpcclienttimeout=30', ...args], { encoding: 'utf8', timeout: 60_000 }).trim();
const a = (...args) => run(A_CLI, ...args);
const b = (...args) => run(B_CLI, ...args);
const aj = (...args) => JSON.parse(a(...args));
const bmm = (n = 1) => { for (let i = 0; i < n; i++) execSync(BMM, { stdio: 'ignore', shell: '/bin/bash', timeout: 120_000 }); };
const aLocked = () => { const w = aj('getwalletinfo'); return w.unlocked_until === 0; };

let seqN = 0;
async function ctl(dir, line, timeoutMs = 120_000) {
  const n = ++seqN;
  try { unlinkSync(join(dir, 'ack')); } catch {}
  writeFileSync(join(dir, '.cmd.tmp'), `${n} ${line}\n`);
  renameSync(join(dir, '.cmd.tmp'), join(dir, 'cmd'));
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutMs) {
    try {
      const r = JSON.parse(readFileSync(join(dir, 'ack'), 'utf8'));
      if (String(r.seq) === String(n)) {
        if (r.err) throw new Error(`desktop: ${line.split(' ')[0]}: ${r.err}`);
        return r.ok;
      }
    } catch (e) {
      if (String(e.message).startsWith('desktop:')) throw e;
    }
    await sleep(100);
  }
  throw new Error(`the desktop didn't answer "${line.split(' ')[0]}"`);
}
const stateOf = (dir) => JSON.parse(readFileSync(join(dir, 'state.json'), 'utf8'));
async function until(what, fn, timeoutMs = 30_000, every = 300) {
  const t0 = Date.now();
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() - t0 > timeoutMs) throw new Error(`timed out waiting for ${what}`);
    await sleep(every);
  }
}

async function qrFromPage(page) {
  // Each path of the QR <svg> in its own colour (the page draws a light "ghost" under the dark squares), as the page's
  // own smoke test reads it, then jsQR.
  const { paths, size } = await page.evaluate(() => {
    const svg = document.querySelector('[data-testid=qr]');
    const paths = [...svg.querySelectorAll('path')].map((p) => ({ d: p.getAttribute('d'), fill: p.getAttribute('fill') || '#000000' }));
    return { paths, size: Number(svg.getAttribute('viewBox').split(' ')[2]) };
  });
  const scale = 6, w = size * scale;
  const px = new Uint8ClampedArray(w * w * 4).fill(255);
  for (const { d, fill } of paths) {
    const [r, g, b] = /^#[0-9a-f]{6}$/i.test(fill) ? [1, 3, 5].map((i) => parseInt(fill.slice(i, i + 2), 16)) : [0, 0, 0];
    for (const [, x, y] of d.matchAll(/M(\d+) (\d+)/g))
      for (let dy = 0; dy < scale; dy++)
        for (let dx = 0; dx < scale; dx++) {
          const o = ((+y * scale + dy) * w + (+x * scale + dx)) * 4;
          px[o] = r;
          px[o + 1] = g;
          px[o + 2] = b;
        }
  }
  return jsQR(px, w, w)?.data ?? null;
}

const results = [];
const check = (what, ok, extra = '') => {
  results.push({ what, ok: !!ok });
  console.log(`  ${ok ? 'ok  ' : 'FAIL'} ${what}${extra ? ' — ' + extra : ''}`);
  if (!ok) throw new Error(`failed: ${what}${extra ? ' — ' + extra : ''}`);
};
mkdirSync(SHOTS, { recursive: true });
let shotN = 0;
const shot = (page, name) => page.screenshot({ path: join(SHOTS, `${String(++shotN).padStart(2, '0')}-${name}.png`) }).catch(() => {});

const browser = await chromium.launch();
const problems = [];
async function phone(name) {
  const ctx = await browser.newContext({ ...devices['Pixel 7'], colorScheme: 'dark' });
  const page = await ctx.newPage();
  const cdp = await ctx.newCDPSession(page);
  await cdp.send('WebAuthn.enable');
  await cdp.send('WebAuthn.addVirtualAuthenticator', {
    options: { protocol: 'ctap2', transport: 'internal', hasResidentKey: true, hasUserVerification: true,
      isUserVerified: true, automaticPresenceSimulation: true },
  });
  page.on('pageerror', (e) => problems.push(`${name} pageerror: ${e.message}`));
  page.on('console', (m) => {
    if (process.env.HOSTED_DEBUG) console.log(`    ${name} console:`, m.type(), m.text());
    if (m.type() === 'error' && !/WebSocket connection to .* failed/.test(m.text())) problems.push(`${name} console: ${m.text()}`);
  });
  return page;
}
// A session that wants Face ID first shows the lock screen: unlock it.
async function unlockIfLocked(page) {
  const lock = page.getByTestId('locked');
  if (await lock.isVisible().catch(() => false)) {
    await page.getByRole('button', { name: 'Unlock with Face ID' }).click();
    await lock.waitFor({ state: 'detached', timeout: 15000 });
  }
}

let ok = false;
try {
  const house = Number(readFileSync(join(WORK, 'house'), 'utf8').trim());
  // ---- the owner's phone: pair, phone sends on, Face ID ----
  const owner = await phone('owner');
  await owner.goto(readFileSync(join(CTL_A, 'pair-url'), 'utf8').trim());
  await owner.getByRole('button', { name: 'Pair', exact: true }).click();
  await owner.getByTestId('home').waitFor({ timeout: 20000 });
  check('owner phone paired with desktop A (auto-allowed)', stateOf(CTL_A).devices.length === 1);
  await ctl(CTL_A, 'send-on');
  check('desktop A: "Let my phone send" on, the wallet locked again', stateOf(CTL_A).phone_send && aLocked());
  await owner.getByRole('button', { name: 'Settings' }).click();
  await owner.getByRole('button', { name: 'Turn on Face ID' }).click();
  await owner.getByRole('button', { name: 'Confirm with Face ID' }).click();
  await owner.getByText(/^On: FreeBank asks for Face ID/).waitFor({ timeout: 15000 });
  check("owner phone: Face ID on (a real passkey, checked by the desktop)", true);
  await owner.getByRole('button', { name: 'Back' }).first().click().catch(() => owner.goBack());
  await owner.getByTestId('home').waitFor({ timeout: 10000 });
  await unlockIfLocked(owner);
  await owner.getByTestId('open-invite').waitFor({ timeout: 20000 });
  check('owner phone: Invite to my house shows (houses-mine finds the house on the real node)', true);

  // ---- invite ----
  await owner.getByTestId('open-invite').click();
  await owner.getByRole('button', { name: 'Show the invite with Face ID' }).click();
  await owner.getByTestId('invite-left').waitFor({ timeout: 15000 });
  const inviteUrl = await qrFromPage(owner);
  const inv = JSON.parse(Buffer.from(inviteUrl.split('#pair=')[1], 'base64url').toString());
  check('invite QR names the house', inv?.h?.house === house, JSON.stringify(inv?.h));
  await shot(owner, 'invite');

  // ---- the shopkeeper's phone joins; the owner allows ----
  const shop = await phone('shop');
  await shop.goto(inviteUrl);
  await shop.getByTestId('join-title').waitFor({ timeout: 15000 });
  await shop.getByLabel("Your name, or your shop's").fill('Corner Shop');
  await shop.getByRole('button', { name: 'Join', exact: true }).click();
  const line = shop.getByTestId('pair-code');
  await line.waitFor({ timeout: 15000 });
  const shopCode = /(\d{3} \d{3})$/.exec((await line.textContent()).trim())?.[1];
  await owner.getByTestId('allow-ask').waitFor({ timeout: 15000 });
  const ownerCode = (await owner.getByTestId('allow-code').textContent()).trim();
  check('the Allow card on the owner phone shows the joiner\'s comparison code', shopCode && shopCode === ownerCode, `${shopCode} / ${ownerCode}`);
  // Two asks per invite (security review L4): a second phone opening the same invite is a second card, and each card
  // warns of the other; a third phone is refused.
  const joinFrom = async (who) => {
    const pg = await phone(who);
    await pg.goto(inviteUrl);
    await pg.getByTestId('join-title').waitFor({ timeout: 15000 });
    await pg.getByLabel("Your name, or your shop's").fill(who);
    await pg.getByRole('button', { name: 'Join', exact: true }).click();
    return pg;
  };
  const intruder = await joinFrom('Intruder');
  await until('two asks for the invite', () => stateOf(CTL_A).hosted_asks.length === 2, 15000);
  await owner.getByTestId('allow-others').waitFor({ timeout: 15000 });
  check('a second phone on one invite: two asks, and the owner\'s card warns of the other', true);
  const third = await joinFrom('Third');
  await sleep(3000);
  check('a third phone on the same invite is refused (still two asks)', stateOf(CTL_A).hosted_asks.length === 2);
  await third.close();
  // Allow the shop's card by its code; decline the other.
  for (let i = 0; i < 3 && (await owner.getByTestId('allow-ask').isVisible().catch(() => false)); i++) {
    const shown = (await owner.getByTestId('allow-code').textContent()).trim();
    await owner.getByRole('button', { name: shown === shopCode ? 'Allow with Face ID' : 'Decline' }).click();
    await sleep(1500);
  }
  await owner.getByTestId('allow-ask').waitFor({ state: 'detached', timeout: 15000 });
  await intruder.close();
  await until('desktop A lists the hosted phone', () => stateOf(CTL_A).hosted.length === 1, 15000);
  check('allowed with the owner\'s Face ID; desktop A lists one hosted phone', true);

  // ---- the wallet is made (node A restarts), Face ID first, the words ----
  await shop.getByTestId('faceid-first').waitFor({ timeout: 30000 });
  await shop.getByRole('button', { name: 'Set up Face ID' }).click();
  await shop.getByRole('button', { name: 'Confirm Face ID' }).click();
  const h0 = await until('the wallet made', () => { const h = stateOf(CTL_A).hosted[0]; return ['words', 'failed'].includes(h.step) && h; }, 180_000, 1000);
  check('desktop A made the wallet (createwallet, encryptwallet, node restarted, the words\' key)', h0.step === 'words', h0.why || '');
  const wallet = `hosted-${h0.id}`;
  check('node A runs again and has the hosted wallet', JSON.parse(a('listwallets')).includes(wallet));
  await shop.getByTestId('word-list').waitFor({ timeout: 60000 });
  const words = await shop.locator('[data-testid=word-list] .w').allTextContents();
  check('the shopkeeper\'s phone shows 24 words', words.length === 24);
  await shot(shop, 'words');
  await shop.getByRole('button', { name: "I've written them down" }).click();
  for (const label of await shop.locator('form label').allTextContents()) {
    const n = Number(/Word (\d+)/.exec(label)?.[1]);
    await shop.getByLabel(label, { exact: true }).fill(words[n - 1]);
  }
  await shop.getByRole('button', { name: 'Confirm with Face ID' }).click();
  await until('joining', () => ['joining', 'ready'].includes(stateOf(CTL_A).hosted[0].step), 20000);
  check('words confirmed with Face ID; joining the house', true);

  // ---- joining: blocks until the member list shows the address active ----
  const member = stateOf(CTL_A).hosted[0].member;
  for (let i = 0; i < 40 && stateOf(CTL_A).hosted[0].step !== 'ready'; i++) {
    bmm(1);
    await sleep(1500);
  }
  const hr = stateOf(CTL_A).hosted[0];
  check('joined: the member list shows the address active', hr.step === 'ready', `${hr.step} ${hr.why || ''}`);
  const active = aj('listhousemembers', String(house)).some((m) => m.address === member && m.active);
  check('listhousemembers on node A agrees', active);
  check('node A\'s wallet locked again after the member change', aLocked());
  await shop.getByTestId('open-shop').waitFor({ timeout: 30000 });
  await unlockIfLocked(shop);
  await shot(shop, 'hosted-home');

  // ---- Shop: a payment QR; the owner pays it with Scan to pay ----
  await shop.getByTestId('open-shop').click();
  for (const k of ['0', '.', '0', '0', '1']) await shop.getByRole('button', { name: k, exact: true }).click();
  await shop.getByRole('button', { name: 'Show the QR code' }).click();
  await shop.getByTestId('shop-asked').waitFor({ timeout: 10000 });
  const payUrl = await qrFromPage(shop);
  const pay = JSON.parse(Buffer.from(payUrl.split('#pay=')[1], 'base64url').toString());
  check('the Shop QR is a payment link to the member address', pay.a === member && pay.h === house && pay.u === 0.001, JSON.stringify(pay));
  await shot(shop, 'shop-qr');
  await owner.getByRole('button', { name: 'Back' }).first().click().catch(() => {});
  await owner.getByTestId('home').waitFor({ timeout: 10000 });
  await unlockIfLocked(owner);
  await owner.getByTestId('open-scanpay').click();
  await owner.getByLabel('Or paste a payment link or address').fill(payUrl);
  await owner.getByRole('button', { name: 'Pay this' }).click();
  await owner.getByTestId('pay-to').waitFor({ timeout: 10000 });
  await owner.getByRole('button', { name: 'Review' }).click();
  await owner.getByRole('button', { name: /^Send .* ECX$/ }).click();
  await owner.getByTestId('note-receipt').waitFor({ timeout: 30000 });
  check('Scan to pay: the owner phone paid 0.001 ECX of notes to the shop', true);
  check('node A\'s wallet locked again after paying', aLocked());
  for (let i = 0; i < 20 && !(await shop.getByTestId('shop-paid').isVisible().catch(() => false)); i++) {
    bmm(1);
    await sleep(5000);
  }
  check('the Shop screen says Paid after a block', await shop.getByTestId('shop-paid').isVisible());
  await shot(shop, 'shop-paid');

  // ---- the shopkeeper pays back (to an address of the owner's that is a member too), Face ID every time ----
  const back = a('getnewaddress', '', 'legacy');
  a('walletpassphrase', readFileSync(A_PASS, 'utf8').trim(), '10');
  a('addhousemembers', String(house), JSON.stringify([back]));
  a('walletlock');
  for (let i = 0; i < 6 && !aj('listhousemembers', String(house)).some((m) => m.address === back && m.active); i++) bmm(1);
  const notesOf = () => aj('listmynotes').filter((n) => n.house_id === house).reduce((t, n) => t + n.units, 0);
  const ownerBefore = notesOf();
  const step = (s) => console.log(`    step: ${s}`);
  step('next customer'); await shop.getByRole('button', { name: 'Next customer' }).click().catch(() => {});
  step('back'); await shop.getByRole('button', { name: 'Back' }).first().click();
  step('hosted home'); await shop.getByTestId('hosted-home').waitFor({ timeout: 10000 });
  await unlockIfLocked(shop);
  step('pay'); await shop.getByRole('button', { name: 'Pay', exact: true }).click();
  step('paste'); await shop.getByLabel('Or paste a payment link or address').fill(back);
  step('pay this'); await shop.getByRole('button', { name: 'Pay this' }).click();
  await shot(shop, 'payback-form');
  step('amount'); await shop.getByLabel('Amount of notes').fill('0.0004');
  step('review'); await shop.getByRole('button', { name: 'Review' }).click();
  await shot(shop, 'payback-review');
  step('send'); await shop.getByRole('button', { name: 'Send with Face ID' }).click();
  step('receipt'); await shop.getByTestId('note-receipt').waitFor({ timeout: 30000 });
  check('the shopkeeper paid 0.0004 ECX of notes back, with Face ID, from his hosted wallet', true);
  bmm(2);
  const ownerNotes = notesOf();
  check('node A\'s wallet got the notes back', ownerNotes === ownerBefore + 40_000, `${ownerBefore} -> ${ownerNotes}`);
  await shop.getByRole('button', { name: 'Done' }).click().catch(() => {});

  // ---- move home (fresh words): desktop B set up as new; the house moves the money to the address B gives ----
  const shopUnits = JSON.parse(run([...A_CLI_RAW, `-rpcwallet=${wallet}`], 'listmynotes')).filter((n) => n.house_id === house).reduce((t, n) => t + n.units, 0);
  await ctl(CTL_B, 'setup-new', 300_000);
  check('desktop B set up as new (its own words, encrypted, node B restarted)', true);
  await shop.getByTestId('hosted-home').waitFor({ timeout: 10000 });
  await unlockIfLocked(shop);
  await shop.getByTestId('open-move').click();
  await shop.getByLabel('Or paste its pairing link').fill(readFileSync(join(CTL_B, 'pair-url'), 'utf8').trim());
  await shop.getByRole('button', { name: 'Pair with this link' }).click();
  await shop.getByTestId('move-tell').waitFor({ timeout: 30000 });
  const target = (await shop.getByTestId('move-target').textContent()).trim();
  const bOwns = JSON.parse(b('getaddressinfo', target)).ismine === true;
  check('the shop phone paired with desktop B, which gave an address of node B\'s wallet', stateOf(CTL_B).devices.length === 1 && bOwns, target);
  await shot(shop, 'move-tell');
  await shop.getByRole('button', { name: 'Move my money with Face ID' }).click();
  await shop.getByTestId('move-moving').waitFor({ timeout: 20000 });
  await until('moving on desktop A', () => ['moving', 'moved'].includes(stateOf(CTL_A).hosted[0].step), 15000);
  check('move-home with Face ID: desktop A is moving the money', true);
  for (let i = 0; i < 60 && stateOf(CTL_A).hosted[0].step !== 'moved'; i++) {
    bmm(1);
    await sleep(3000);
  }
  const hm = stateOf(CTL_A).hosted[0];
  check('desktop A: moved, the copy empty', hm.step === 'moved' && hm.empty === true, `${hm.step} ${hm.why || ''}`);
  check('desktop A heard "moved" (and its owner is asked about the copy)', stateOf(CTL_A).moved_events === 1);
  await shop.getByTestId('move-done').waitFor({ timeout: 30000 });
  check("the phone says to replace the shop's QR codes", (await shop.getByTestId('move-done').textContent()).includes('QR codes'));
  await shop.getByRole('button', { name: 'Done' }).click();
  await shop.getByTestId('home').waitFor({ timeout: 30000 });
  await unlockIfLocked(shop);
  await shot(shop, 'moved-home');
  const hostedCli = [...A_CLI_RAW, `-rpcwallet=${wallet}`];
  const leftNotes = JSON.parse(run(hostedCli, 'listmynotes')).reduce((t, n) => t + n.units, 0);
  const leftEcx = Number(run(hostedCli, 'getbalance')) + Number(run(hostedCli, 'getunconfirmedbalance'));
  check('node A\'s hosted wallet ends empty (no notes, no ECX)', leftNotes === 0 && leftEcx === 0, `${leftNotes} units, ${leftEcx} ECX`);
  const targetActive = aj('listhousemembers', String(house)).some((m) => m.address === target && m.active);
  check('the address B gave is an active member of the house', targetActive);
  // After the move the old address comes off the house (the re-review of v0.2.8, N2).
  for (let i = 0; i < 20 && aj('listhousemembers', String(house)).some((m) => m.address === member && m.active); i++) {
    bmm(1);
    await sleep(3000);
  }
  check('the old member address is no longer active at the house', !aj('listhousemembers', String(house)).some((m) => m.address === member && m.active));
  const bNotes = JSON.parse(b('listmynotes')).filter((n) => n.house_id === house).reduce((t, n) => t + n.units, 0);
  check('node B holds the shopkeeper\'s notes', bNotes === shopUnits, `${bNotes} of ${shopUnits}`);
  const atTarget = JSON.parse(b('listunspent', '0', '9999999', JSON.stringify([target])));
  const bEcx = atTarget.reduce((t, u) => t + u.amount, 0);
  check('node B holds the ECX at the address its desktop gave', bEcx > 0, `${bEcx} ECX in ${atTarget.length} coins`);
  const members = await ctl(CTL_B, 'members');
  check("desktop B's member addresses list it (Receive on the desktop)", members.some((m) => m.house === house && m.address === target), JSON.stringify(members));
  await shop.getByRole('button', { name: 'Receive', exact: true }).click();
  await shop.getByTestId('member-address').waitFor({ timeout: 20000 });
  const shown = await shop.getByTestId('member-address').textContent();
  check("the phone's Receive (now desktop B's) shows it as the address at the house", shown.includes(target));
  await shot(shop, 'receive-member');

  // ---- Delete the copy: at node A's next start the emptied file and its passphrase are deleted outright ----
  await ctl(CTL_A, `hosted-remove ${h0.id}`);
  check('desktop A deleted its copy', stateOf(CTL_A).hosted[0].remove === true);
  a('stop');
  await until('node A stopped', () => { try { a('getblockcount'); return false; } catch { return true; } }, 60000, 1000);
  const wfile = execSync(`find ${A_DATADIR} -maxdepth 3 -name ${wallet}`, { encoding: 'utf8' }).trim().split('\n')[0];
  check('the hosted wallet file was in node A\'s wallet folder', !!wfile, wfile);
  await ctl(CTL_A, `sweep ${join(A_DATADIR, 'regtest')}`);
  execSync(A_RESTART, { stdio: 'ignore', shell: '/bin/bash', timeout: 120_000 });
  const keysFile = join(WORK, 'app-a', 'phone', 'hosted-keys.json');
  const keys = existsSync(keysFile) ? JSON.parse(readFileSync(keysFile, 'utf8')).keys : [];
  const aside = existsSync(join(WORK, 'app-a', 'hosted-removed')) ? readdirSync(join(WORK, 'app-a', 'hosted-removed')) : [];
  check('at node A\'s start the emptied file was deleted outright, its passphrase too, nothing moved aside',
    !existsSync(wfile) && !keys.some((k) => k.id === h0.id) && aside.length === 0, `aside: ${aside.join(', ')}`);
  check('no page errors', problems.length === 0, problems.join(' | '));
  ok = true;
} catch (e) {
  console.log('  STOPPED: ' + e.message.split('\n')[0]);
  if (problems.length) console.log('  page problems: ' + problems.join(' | '));
} finally {
  await browser.close();
}
console.log(`${results.filter((r) => r.ok).length}/${results.length} checks passed${ok ? '' : ' (stopped early)'}`);
process.exit(ok ? 0 : 1);
