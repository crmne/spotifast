// Spotifast Ribbon: the opening, and the square that shows the album art or
// the visualizer. The engine's Date runs on the wall clock, which a skin's
// preview does not, so all timing comes from `clock`, an element the engine
// moves one pixel a millisecond. `ticker` moves too and calls back each frame
// while anything animates, and stands still otherwise, so that an idle skin
// costs nothing.

var PLAYING = 3, BUFFERING = 6, WAITING = 7, TRANSITIONING = 9;

// The opening, in milliseconds. It holds a moment first, so that the core is
// seen alone whatever the window took to come up.
var HOLD = 300, SWEEP_FROM = 200, SWEEP_TO = 1100, LIGHT_AT = 1100, UI_AT = 1350;
var REVEAL_AT = 1650, OPEN_END = 2100;
var LAST_FRAME = 13, SIDE = 126, FOREVER = 600000;

var running = false, last = 0;
var opening = true, openAt = 0;
var closing = false, closeAt = 0;
var lightOn = false, uiOn = false, revealed = false;
var showVisual = false, swapping = false, veilOpen = false;

function boot() {
    showVisual = theme.loadPreference("pane") == "visual";
    core.alphaBlendTo(255, 200);
    veilSlot.alphaBlendTo(255, 200);
    sweep.visible = true;
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
        openAt += passed;
        playOpening(openAt - HOLD);
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

function showRibbon(share) {
    sweep.value = Math.round(share * LAST_FRAME);
}

function holdRibbon(still) {
    ribbon.visible = still;
    sweep.visible = !still;
}

function playOpening(t) {
    var swept = smooth(part(t, SWEEP_FROM, SWEEP_TO));
    showRibbon(swept);
    if (swept >= 1 && !ribbon.visible) holdRibbon(true);
    if (!lightOn && t >= LIGHT_AT) {
        lightOn = true;
        showLight(500);
    }
    if (!uiOn && t >= UI_AT) {
        uiOn = true;
        ui.alphaBlendTo(255, 350);
        pill.alphaBlendTo(255, 350);
    }
    if (!revealed && t >= REVEAL_AT) {
        revealed = true;
        showPane();
        followVeil();
    }
    if (t >= OPEN_END) opening = false;
}

function playClosing(t) {
    showRibbon(1 - smooth(part(t, 240, 640)));
    if (t >= 240 && ribbon.visible) holdRibbon(false);
    if (t >= 720) {
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

function showPane() {
    paneBack.visible = true;
    art.visible = !showVisual;
    fx.visible = showVisual;
}

// With nothing on, the glass closes over the square.
function followVeil() {
    var wanted = hasSong();
    if (wanted != veilOpen) {
        veilOpen = wanted;
        veil.slideTo(0, wanted ? -SIDE : 0, 420);
    }
}

function followPlayer() {
    var isPlaying = sounding();
    playButton.visible = !isPlaying;
    pauseButton.visible = isPlaying;
    wave.visible = isPlaying;
    var song = hasSong();
    title.value = song ? player.currentMedia.name : "Nothing playing";
    artist.value = song ? player.currentMedia.getItemInfo("Artist") : "";
    title.scrolling = title.textWidth > title.width;
    artist.scrolling = artist.textWidth > artist.width;
    if (opening) return;
    if (closing) return;
    showLight(400);
    if (!swapping) followVeil();
}

// Clicking the square, or its button, draws the glass across, changes what is
// behind it and opens again.
function togglePane() {
    showVisual = !showVisual;
    theme.savePreference("pane", showVisual ? "visual" : "art");
    if (opening || closing) return;
    if (!hasSong()) {
        showPane();
        return;
    }
    swapping = true;
    veilOpen = false;
    veil.slideTo(0, 0, 170);
}

function veilArrived() {
    if (!swapping) return;
    swapping = false;
    showPane();
    followVeil();
}

// Closing runs the opening backwards, faster, and then goes.
function shutDown() {
    if (closing) return;
    closing = true;
    opening = false;
    closeAt = 0;
    veil.slideTo(0, 0, 200);
    ui.alphaBlendTo(0, 200);
    pill.alphaBlendTo(0, 200);
    light.alphaBlendTo(0, 240);
    animate();
}
