// The ribbon skins, tall and wide: the opening, the square that shows the album art or the
// visualizer, and the pages of the glass. The engine's Date runs on the wall
// clock, which a skin's preview does not, so all timing comes from `clock`, an
// element the engine moves one pixel a millisecond. `ticker` moves too and
// calls back each frame while anything animates, and stands still otherwise,
// so that an idle skin costs nothing.

var PLAYING = 3, BUFFERING = 6, WAITING = 7, TRANSITIONING = 9;

// The opening, in milliseconds: light, then the object, then its screen.
var TRACE_TO = 600, WIPE_TO = 840, WAKE_AT = 840, REVEAL_AT = 1050, OPEN_END = 1520;
// The app says when the window is up and every picture is in. A window that
// never takes the skin's size must not leave it dark.
var LONGEST_WAIT = 1500;
var TRACE_LAST = 13, WIPE_LAST = 7, FOREVER = 600000;
// Not EQ or QUEUE: names are not case sensitive, and those are elements.
var PAGE_NOW = 0, PAGE_EQ = 1, PAGE_QUEUE = 2;

var running = false, last = 0, waited = 0;
var opening = true, openAt = 0;
var closing = false, closeAt = 0;
var woken = false, revealed = false, whole = false;
var showVisual = false, page = PAGE_NOW, changing = false, veilOpen = false;

function boot() {
    showVisual = theme.loadPreference(REMEMBER + "pane") == "visual";
    var saved = theme.loadPreference(REMEMBER + "page");
    if (saved == "eq") page = PAGE_EQ;
    if (saved == "queue") page = PAGE_QUEUE;
    showDots();
    followPlayer();
    animate();
}

function animate() {
    if (running) return;
    running = true;
    last = 0;
    clock.left = 0;
    clock.moveTo(FOREVER, 0, FOREVER);
    step();
}

function step() {
    ticker.moveTo(ticker.left > 0 ? 0 : 1, 0, 12);
}

function tick() {
    var now = clock.left, passed = now - last;
    last = now;
    var more = false;
    if (closing) {
        closeAt += passed;
        more = playClosing(closeAt);
    } else if (opening) {
        if (view.onScreen === false && waited < LONGEST_WAIT) {
            waited += passed;
        } else {
            openAt += passed;
            playOpening(openAt);
        }
        more = opening;
    }
    if (more) {
        step();
    } else {
        running = false;
        clock.moveTo(0, 0, 0);
    }
}

function part(t, from, to) {
    if (t <= from) return 0;
    if (t >= to) return 1;
    return (t - from) / (to - from);
}

function smooth(share) {
    return share * share * (3 - 2 * share);
}

function showOpening(traced, wiped) {
    trace.value = Math.round(traced * TRACE_LAST);
    wipe.value = Math.round(wiped * WIPE_LAST);
}

// The object is either the wipe's pictures or itself, never parts of both.
function holdWhole(on) {
    whole = on;
    object.alphaBlend = on ? 255 : 1;
    trace.visible = !on;
    wipe.visible = !on;
}

function playOpening(t) {
    showOpening(part(t, 0, TRACE_TO), smooth(part(t, TRACE_TO, WIPE_TO)));
    if (t >= WIPE_TO && !whole) {
        light.alphaBlend = 255;
        holdWhole(true);
    }
    if (!woken && t >= WAKE_AT) {
        woken = true;
        showLight(500);
        screen.alphaBlendTo(255, 300);
        pill.alphaBlendTo(255, 300);
    }
    if (!revealed && t >= REVEAL_AT) {
        revealed = true;
        showPage();
        followVeil();
    }
    if (t >= OPEN_END) opening = false;
}

function playClosing(t) {
    if (t >= 180 && whole) holdWhole(false);
    showOpening(1 - part(t, 340, 620), 1 - smooth(part(t, 180, 340)));
    if (t >= 640) {
        view.close();
        return false;
    }
    return true;
}

function hasSong() {
    return player.currentMedia.name != "";
}

function sounding() {
    var state = player.playState;
    return state == PLAYING || state == BUFFERING || state == WAITING || state == TRANSITIONING;
}

// The light under the arch burns while the music plays.
function showLight(ms) {
    light.alphaBlendTo(sounding() ? 255 : 70, ms);
}

// Which page is up, and in the square what the choice says.
function showPage() {
    paneBack.visible = true;
    art.visible = page == PAGE_NOW && !showVisual;
    var filling = page == PAGE_NOW && showVisual && hasSong();
    if (filling != visual.visible) {
        visual.visible = filling;
        if (filling) {
            visual.alphaBlend = 0;
            visual.alphaBlendTo(255, 220);
        }
    }
    wave.visible = sounding() && !showVisual;
    nowPage.visible = page == PAGE_NOW;
    eqPage.visible = page == PAGE_EQ;
    queuePage.visible = page == PAGE_QUEUE;
}

// Glass covers the square on the other pages, and with nothing on.
function followVeil() {
    var wanted = page == PAGE_NOW && hasSong() && !showVisual;
    if (wanted != veilOpen) {
        veilOpen = wanted;
        veil.slideTo(0, wanted ? -SIDE : 0, 320);
    }
}

function followPlayer() {
    var isPlaying = sounding();
    playButton.visible = !isPlaying;
    pauseButton.visible = isPlaying;
    var song = hasSong();
    title.value = song ? player.currentMedia.name : "Nothing playing";
    artist.value = song ? player.currentMedia.getItemInfo("Artist") : "";
    title.scrolling = title.textWidth > title.width;
    artist.scrolling = artist.textWidth > artist.width;
    if (opening || closing) return;
    showLight(400);
    if (changing) return;
    showPage();
    followVeil();
}

// A change of what the square shows, or of page, happens behind the glass.
function change() {
    if (opening || closing) return;
    if (!veilOpen) {
        showPage();
        followVeil();
        return;
    }
    changing = true;
    veilOpen = false;
    veil.slideTo(0, 0, 150);
}

function veilArrived() {
    if (!changing) return;
    changing = false;
    showPage();
    followVeil();
}

function togglePane() {
    showVisual = !showVisual;
    theme.savePreference(REMEMBER + "pane", showVisual ? "visual" : "art");
    change();
}

function setPage(wanted) {
    page = wanted;
    theme.savePreference(REMEMBER + "page", page == PAGE_EQ ? "eq" : page == PAGE_QUEUE ? "queue" : "now");
    showDots();
    change();
}

function showDots() {
    var dots = "page" + page + ".png";
    pageButton.image = dots;
    pageButton.hoverImage = dots;
    pageButton.downImage = dots;
}

function nextPage() {
    setPage((page + 1) % 3);
}

// One click on a row plays it.
function playRow() {
    var row = queue.selectedItem;
    if (row >= 0) player.controls.playItem(player.currentPlaylist.item(row));
    queue.selectedItem = -1;
}

// Closing runs the opening backwards, faster, and then goes.
function shutDown() {
    if (closing) return;
    closing = true;
    opening = false;
    closeAt = 0;
    veil.slideTo(0, 0, 150);
    visual.alphaBlendTo(0, 150);
    screen.alphaBlendTo(0, 160);
    pill.alphaBlendTo(0, 160);
    animate();
}
