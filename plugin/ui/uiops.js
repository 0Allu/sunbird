// uiops.js - codec for Phoenix (Discord native Rust client) UI-op batches,
// as consumed by com.discord.nativeapp.modules.gq.d([BI)V (UIManager), and the
// view-event frames Java sends back via WasmRuntime.nativeDispatch(J[B)Z.
// Plain ES5-ish JS, no imports; works in QuickJS and Node. See FORMAT.md.
(function () {
  'use strict';
  // lh/c2.a: prop/event name table, index == u16 wire key (855 entries, versionCode 356207)
  var PROPS = 'absolute accessibilityActions accessibilityBusy accessibilityChecked accessibilityDisabled accessibilityExpanded accessibilityFocus accessibilityHidden accessibilityHint accessibilityId accessibilityKeepsChildren accessibilityLabel accessibilityModal accessibilitySelected accessibilityTapLabel accessibilityText accessibilityValue accessible activation activeAbsolute activeAlign activeBackground activeBackgroundGradient activeBackgroundGradientAxis activeBlur activeBorderColor activeBorderLeft activeBorderSides activeBorderWidth activeClip activeDirection activeFlex activeFlexBasis activeGap activeGapX activeGapY activeGravity activeHairline activeHeight activeHidden activeInsetPct activeJustify activeMargin activeMarginAuto activeMaxHeight activeMaxWidth activeMinHeight activeMinWidth activeMotion activeOpacity activePadding activePaddingBox activeRadius activeRadiusBottomLeft activeRadiusBottomRight activeRadiusTopLeft activeRadiusTopRight activeRotation activeShadow activeShrink activeWidth advance after align alignment allowFontScaling along alpha always amount angle animate animateLayout animated announce announcePolite any artboard at autoCapitalize autoCorrect autoDismissMs autoScroll autoSplit autofill autoplay axis back backPreview backdropBlur background backgroundGradient backgroundGradientAxis bezier bg bgBorder bgBorderWidth bgFitText bgRadius bindings block blocksOuterPager blur bold borderColor borderLeft borderSides borderWidth bottom bottomOffset bounces bounds buffer cacheKey cancels cardOverlay cardPress carry centre chrome circle clamp clickableImage clip clipOutline close closeMs closed code color colorFilters colorSpace combine combineTarget combines commit count cropZoom cross crossMax crossMin curve damping date decide delay dim direction disallowInterruption dismiss dismissDrag dismissLabel dismissible displace distance doubleTapZoom dragDismiss dragLocked dragOffset dragScale dragToSelect draggable drags drop duration dx dxCenter dxOwn dy dyOwn easing edge edgeInset elevation ellipsize enabled end endLoading enter enterContent enterFrom entry epoch exit expanded extend factor fade fadeMotion fadeMs fadeQuick fallback fastestTop fill first firstLineIndent fit fixed flex flexBasis fling focusBorderColor focusable follow follower font fontFamily fontSize fontWeight forceValue frameId frameName free from fromDown fromStart fullBasis fullResolution fullScreenBack fullscreen gap gapX gapY gestureExclusion gone grab gradient gravity hairline handleColor handleHidden handleOverlay haptic haptics hardwareEnterSubmits height hidden hint hitSlop holdCancel holdMs holdZones holds holes hoverAt icon id idle idleMs image imagePool includeFontPadding index input insetPct insetSafeArea intensity italic items iterations justify k keepKeyboard keyboard keyboardAvoid keyboardAvoidKeepInset keyboardAvoidMin keyboardAvoidMotion keyboardAvoidMove keyboardAware keyboardDismiss keyboardDismissFollows keyboardShouldPersistTaps kick kind label length letterSpacing lineHeight lineHeightExact lineSpacingMult liveRegion longPressDuration longPressHaptic loop looping mana margin marginAuto maskGradient mass maxDate maxHeight maxLines maxStart maxTrackTint maxWidth maxZoom minDate minHeight minTrackTint minWidth mirror mirrored mode motion motionOut motions move moveMotion ms multiline muted name noExtractUi noPressHold offscreenAlpha offset offsetX offsetY on once opacity open ops out output outside over overscroll overshootFriction ownCurve padding paddingBox page pageRelease pageStep paging panSnap panYieldsToScroll panel panelBack parityExplicitLabel parityLabel parityNode passthrough pauseAfterMs pauseOnDrag paused peak period pinch pinned pixelate placeholder placeholderColor placeholderPool placement platformText play playFinished playing plus pointerEvents points pointsJump pool pop popover popoverArrow portal position poster presentation pressCell pressDelay pressFeedback pressInputs pressReplay pressRipple pressScale pressStyle pressable pressed progress projection property props radius radiusBottomLeft radiusBottomRight radiusTopLeft radiusTopRight rangeInsets rearm rects referencedAssets refreshing releaseMs reorder repeat replace request reset resistance resizeMode rest restProgress restoreMs retarget retractAfter returnKey reveal reverse role rotate rotateJitter rotation round roundBitmap roundEachCell rowAccessibilityActions rowActions rowAnimation rowChanges rowReveal rowSwipe rubber scale scaleFactor scaleX scaleY scrim scrimColor scrimOpacity scrollIndicator scrollIndicatorInsets scrollLink scrollLocked scrollName scrollOffset scrollSettle scrollToEnd scrollTuck scrollable secure seekTo selectable selection selectionFormats seq sequence settle shadow sharpness sheetClear sheetClip sheetLayer sheetMaxHeight shift shrink sidewaysTo singleLine size slop snap snapMs span spans speed spring src stage stages start startHaptic startHeight starts stateMachine statusBarHidden stepHaptic steps stickToEnd stickyHover stiffness still streamId strike stroke style sweep sweepFill swipeName swipeStages swipeable systemOverlay tap target tension text textAlign textAlignVertical textBreakStrategy textGradient textGradientShape textInputState textShadow textStroke textTransform theme thickness threshold thumbSize thumbTint tickFormat tickFrom tie time timeline timerLink times timing tint title to token top topAvoid toss touchable toward track trackFrame transition transitionName translateX translateY translateYSteps truncationLabel type underline unit uri value valueSpans values velocity via view viewModelInstance visible weight wide width widthMotion windowAlign windowDrag windowSize x y yieldFrom yieldScroll zIndex zone zones zoomMotion zoomTo above accessibilityAction active appeared atEnd baseline bottomAvoid catchUp change changeText colors content drag dragging dropHaptic endOffset ended focus focusX focusY foldOffset foldScale ghost hold inset interacting key keyPress layout left lift link load loadProgress loaded menu min navigate off opacityKeyframes own pageDrag pageSelect pageSettle pan popped positions press pressDown px radial range reducedMotion rowPress scaleKeyframes screenEnd screenStart scrollView scrolling selectionChange state submit textTruncated total touchX touchY translateXOwn truncated viewport vx vy zoomed doubleTap pressOut pressIn longPress error now max row copy loopPass keypath ready linkPress accessibilityTap url accessibilityEscape rotateKeyframes scrimTap translateXKeyframes translateYKeyframes buffering listTap pageScroll paste pasteText placed refresh rowAccessibilityAction rowCombine rowDragHover rowMove rowPressRelease rowPressDown rowLongPress rowDoubleTap rowDragStart setSelectionFormat startReached videoSize visibility linkLongPress cropRect file files fold held interactionData onto all keepAwake longPressOnly accessibilityAboveSheets sheetPreventDismiss handle mime touchOnly accessibilityRoleDescription holdLanding exitEnd revealEnd zoomControl passTaps spec box groups layers origin anims prop keys ease phase path group x0 x1 stops motionOnly rowLongPressTouchOnly pageSheet linkName linkAttachment nativeID accessibilityLabelledBy borderStyle shadowOutset mux rowActionsTarget backdrop ramp flareHeight flareWidth insetDevicePx collapse worklet panWorklet panWorkletAxis portalKey portalSlot pinchWorklet panWorkletConfig viewMotion reach gestureTag gestureRelations scrollFade paragraphs largest panByMedia leadingIcon firstFrame bgShadow bgInsetY startAfterScroll opened scrubKey scrubs cameraZoomSlot statusBarLight blurSpread returnSubmits elevationColor linkAction pressMaxMovement doubleTapDelay keepRipple keyboardDismissChatInput contentBlur noCollection startsMediaSession accessibilityValueSeconds stackHeader underSheets valueRange overlayBackdrop gradientMask alphas maxFontSizeMultiplier accessibilityNamedByChildren roleDot expandSeq draw drawEdits wholePointHeight contentCenter accessibilityFocusScreen faded textSteps scaledInputs stepInput videoTracks accessibilityPosition throttled accessibilityGroup rowPlaceholder rowPlaceholderRows pinnedHeader tapToSeek edgeInsetLeft maintainVisibleContentPosition alwaysBounce tabularNumbers uikitPush windowOverlay modal longPressTouchOnly exactHitArea tickSuffix buffered fraction hitSlopEdges referencedFonts eventCount linkRole navScrimColor accessibilityLinksHidden restStart restEnd inPlace visibleFraction offsets caretHidden readerLock gradientShape accessibilityGrid accessibilityGridCell accessibilityPositionsFromEnd sheetNoBottomInset pinchWorkletConfig spellCheck keepBasis belowBaseline topOnly revealIfUnlaid paneTitle wholeSource notoFallback lineBreakStrategyIOS centerEachLine swipeShadow holdPresses generation outset maxLength blocksBack selectionColor rowsSettled roundLineHeights wrapToEdge appended yyTextBounds endsPassOnHandOff loadingBackground wholePointCode basisShrinks lineSpacingExtra'.split(' ');
  var PROP_ID = {};
  for (var i = 0; i < PROPS.length; i++) PROP_ID[PROPS[i]] = i;
  // modules/rq.f: op names, index == op byte
  var OPS = ['', 'create', 'update', 'children', 'delete', 'root', 'focus', 'scrollTo', 'requestFrame', 'motion', 'childEdits'];
  var OP_ID = {};
  for (i = 1; i < OPS.length; i++) OP_ID[OPS[i]] = i;
  // value tags (yq.f / debugmeta.c.Q)
  var TAGS = ['null', 'false', 'true', 'i32', 'i64', 'f64', 'str', 'arr', 'obj', 'rect', 'map', 'words', 'wpatch'];
  var TAG_ID = {};
  for (i = 0; i < TAGS.length; i++) TAG_ID[TAGS[i]] = i;

  function utf8dec(u8, p, n) {
    var s = '', end = p + n;
    while (p < end) {
      var c = u8[p++];
      if (c >= 0xf0) c = ((c & 7) << 18) | ((u8[p++] & 63) << 12) | ((u8[p++] & 63) << 6) | (u8[p++] & 63);
      else if (c >= 0xe0) c = ((c & 15) << 12) | ((u8[p++] & 63) << 6) | (u8[p++] & 63);
      else if (c >= 0xc0) c = ((c & 31) << 6) | (u8[p++] & 63);
      s += String.fromCodePoint(c);
    }
    return s;
  }
  function utf8enc(s) {
    var o = [];
    for (var i = 0; i < s.length; i++) {
      var c = s.codePointAt(i);
      if (c > 0xffff) i++;
      if (c < 0x80) o.push(c);
      else if (c < 0x800) o.push(0xc0 | c >> 6, 0x80 | c & 63);
      else if (c < 0x10000) o.push(0xe0 | c >> 12, 0x80 | c >> 6 & 63, 0x80 | c & 63);
      else o.push(0xf0 | c >> 18, 0x80 | c >> 12 & 63, 0x80 | c >> 6 & 63, 0x80 | c & 63);
    }
    return o;
  }

  function Reader(u8) {
    this.u8 = u8; this.p = 0;
    this.dv = new DataView(u8.buffer, u8.byteOffset, u8.byteLength);
  }
  Reader.prototype = {
    need: function (n) { if (this.p + n > this.u8.length) throw new Error('underflow at ' + this.p); },
    u8_: function () { this.need(1); return this.u8[this.p++]; },
    u16: function () { this.need(2); var v = this.dv.getUint16(this.p, true); this.p += 2; return v; },
    i32: function () { this.need(4); var v = this.dv.getInt32(this.p, true); this.p += 4; return v; },
    i64: function () { this.need(8); var v = this.dv.getBigInt64(this.p, true); this.p += 8; return v; },
    f32: function () { this.need(4); var v = this.dv.getFloat32(this.p, true); this.p += 4; return v; },
    f64: function () { this.need(8); var v = this.dv.getFloat64(this.p, true); this.p += 8; return v; },
    str: function (n) { this.need(n); var s = utf8dec(this.u8, this.p, n); this.p += n; return s; }
  };

  function Writer() { this.b = []; this.t = new DataView(new ArrayBuffer(8)); }
  Writer.prototype = {
    u8_: function (v) { this.b.push(v & 255); },
    raw: function (n) { for (var i = 0; i < n; i++) this.b.push(this.t.getUint8(i)); },
    u16: function (v) { this.t.setUint16(0, v, true); this.raw(2); },
    i32: function (v) { this.t.setInt32(0, v, true); this.raw(4); },
    i64: function (v) { this.t.setBigInt64(0, BigInt(v), true); this.raw(8); },
    f32: function (v) { this.t.setFloat32(0, v, true); this.raw(4); },
    f64: function (v) { this.t.setFloat64(0, v, true); this.raw(8); },
    bytes: function (a) { for (var i = 0; i < a.length; i++) this.b.push(a[i]); },
    str32: function (s) { var e = utf8enc(s); this.i32(e.length); this.bytes(e); },
    out: function () { return new Uint8Array(this.b); }
  };

  function keyName(k) { return k < PROPS.length ? PROPS[k] : k; }
  function keyId(k) {
    if (typeof k === 'number') return k;
    if (!(k in PROP_ID)) throw new Error('unknown prop ' + k);
    return PROP_ID[k];
  }

  // Value = {t: tagName, v: payload}. arr: v=[Value]; obj: v=[{k,v:Value}];
  // map: v=[{k:string,v:Value}]; rect: v=[f32 x4]; words: v=[i32];
  // wpatch: v={list:i32, set:[[u16 idx, i32 word]]}; i64 is a BigInt.
  function readVal(r, tag) {
    switch (tag) {
      case 0: return { t: 'null' };
      case 1: return { t: 'false' };
      case 2: return { t: 'true' };
      case 3: return { t: 'i32', v: r.i32() };
      case 4: return { t: 'i64', v: r.i64() };
      case 5: return { t: 'f64', v: r.f64() };
      case 6: return { t: 'str', v: r.str(r.i32()) };
      case 7: { var n = r.i32(), a = []; while (n--) a.push(readVal(r, r.u8_())); return { t: 'arr', v: a }; }
      case 8: { n = r.i32(); a = []; while (n--) { var k = r.u16(); a.push({ k: keyName(k), v: readVal(r, r.u8_()) }); } return { t: 'obj', v: a }; }
      case 9: return { t: 'rect', v: [r.f32(), r.f32(), r.f32(), r.f32()] };
      case 10: { n = r.i32(); a = []; while (n--) { k = r.str(r.i32()); a.push({ k: k, v: readVal(r, r.u8_()) }); } return { t: 'map', v: a }; }
      case 11: { n = r.i32(); a = []; while (n--) a.push(r.i32()); return { t: 'words', v: a }; }
      case 12: { var list = r.i32(); n = r.u16(); a = []; while (n--) a.push([r.u16(), r.i32()]); return { t: 'wpatch', v: { list: list, set: a } }; }
    }
    throw new Error('unknown value tag ' + tag + ' at ' + (r.p - 1));
  }
  function writeVal(w, val) {
    var tag = TAG_ID[val.t], v = val.v, i;
    if (tag === undefined) throw new Error('bad value type ' + val.t);
    w.u8_(tag);
    switch (tag) {
      case 3: w.i32(v); break;
      case 4: w.i64(v); break;
      case 5: w.f64(v); break;
      case 6: w.str32(v); break;
      case 7: w.i32(v.length); for (i = 0; i < v.length; i++) writeVal(w, v[i]); break;
      case 8: w.i32(v.length); for (i = 0; i < v.length; i++) { w.u16(keyId(v[i].k)); writeVal(w, v[i].v); } break;
      case 9: for (i = 0; i < 4; i++) w.f32(v[i]); break;
      case 10: w.i32(v.length); for (i = 0; i < v.length; i++) { w.str32(v[i].k); writeVal(w, v[i].v); } break;
      case 11: w.i32(v.length); for (i = 0; i < v.length; i++) w.i32(v[i]); break;
      case 12: w.i32(v.list); w.u16(v.set.length); for (i = 0; i < v.set.length; i++) { w.u16(v.set[i][0]); w.i32(v.set[i][1]); } break;
    }
  }
  // props block (yq.b): u16 count, then {u16 key, u8 tag, value}
  function readProps(r) {
    var n = r.u16(), a = [];
    while (n--) { var k = r.u16(); a.push({ k: keyName(k), v: readVal(r, r.u8_()) }); }
    return a;
  }
  function writeProps(w, a) {
    w.u16(a.length);
    for (var i = 0; i < a.length; i++) { w.u16(keyId(a[i].k)); writeVal(w, a[i].v); }
  }
  function ints(r) { var n = r.i32(), a = []; while (n--) a.push(r.i32()); return a; }
  function wints(w, a) { w.i32(a.length); for (var i = 0; i < a.length; i++) w.i32(a[i]); }

  // batch = u8 version(1) + ops until end of buffer
  function decode(u8) {
    var r = new Reader(u8), ops = [];
    var ver = r.u8_();
    if (ver !== 1) throw new Error('view wire version ' + ver);
    while (r.p < u8.length) {
      var at = r.p, code = r.u8_(), op;
      switch (code) {
        case 1: { var id = r.i32(); op = { op: 'create', id: id, type: r.str(r.u16()), props: readProps(r) }; break; }
        case 2: { id = r.i32(); op = { op: 'update', id: id, props: readProps(r) }; break; }
        case 3: { id = r.i32(); op = { op: 'children', id: id, ids: ints(r) }; break; }
        case 4: op = { op: 'delete', ids: ints(r) }; break;
        case 5: op = { op: 'root', id: r.i32() }; break;
        case 6: op = { op: 'focus', id: r.u8_() ? r.i32() : null }; break;
        case 7: { id = r.i32(); op = { op: 'scrollTo', id: id, v: readVal(r, r.u8_()) }; break; }
        case 8: op = { op: 'requestFrame' }; break;
        case 9: op = { op: 'motion', words: ints(r) }; break;
        case 10: {
          id = r.i32();
          var n = r.i32(), edits = [];
          while (n--) { var keep = r.i32(), del = r.i32(); edits.push({ keep: keep, del: del, ins: ints(r) }); }
          op = { op: 'childEdits', id: id, edits: edits };
          break;
        }
        default: throw new Error('unknown op ' + code + ' at ' + at);
      }
      ops.push(op);
    }
    return ops;
  }

  function encode(ops) {
    var w = new Writer();
    w.u8_(1);
    for (var i = 0; i < ops.length; i++) {
      var o = ops[i], code = OP_ID[o.op];
      if (!code) throw new Error('unknown op ' + o.op);
      w.u8_(code);
      switch (code) {
        case 1: { w.i32(o.id); var t = utf8enc(o.type); w.u16(t.length); w.bytes(t); writeProps(w, o.props); break; }
        case 2: w.i32(o.id); writeProps(w, o.props); break;
        case 3: w.i32(o.id); wints(w, o.ids); break;
        case 4: wints(w, o.ids); break;
        case 5: w.i32(o.id); break;
        case 6: if (o.id == null) w.u8_(0); else { w.u8_(1); w.i32(o.id); } break;
        case 7: w.i32(o.id); writeVal(w, o.v); break;
        case 8: break;
        case 9: wints(w, o.words); break;
        case 10:
          w.i32(o.id); w.i32(o.edits.length);
          for (var j = 0; j < o.edits.length; j++) { var e = o.edits[j]; w.i32(e.keep); w.i32(e.del); wints(w, e.ins); }
          break;
      }
    }
    return w.out();
  }

  // ---- outgoing view events (Java -> Rust via nativeDispatch) ----
  // view frame (yq.d + GuestHost.d): 00 00 00 00 | 01 01 | i32 node | u16 eventKey | tagged value
  // batch frame (f7.t.d): 04 00 | u16 0 | u16 0 | u16 0 | u32 0 | u32 hdrLen(0) | hdr | {u32 len, frame}*
  // module frame (GuestHost.c): 03 00 | u16 0 | u16 eventId | u16 0 | u32 0 | u32 hdrLen | hdr | body
  // decodeDispatch returns a flat list; each item has {off,len} (byte range in the input) so a
  // caller can splice it out. View events get {kind:'view', node, event, payload}.
  function decodeDispatch(u8) {
    var out = [];
    walk(u8, 0, u8.length, out);
    return out;
  }
  function walk(u8, off, len, out) {
    var dv = new DataView(u8.buffer, u8.byteOffset, u8.byteLength);
    var kind = u8[off];
    if (kind === 4) {
      var hl = dv.getUint32(off + 12, true), p = off + 16 + hl, end = off + len;
      while (p + 4 <= end) {
        var n = dv.getUint32(p, true);
        walk(u8, p + 4, n, out);
        p += 4 + n;
      }
      return;
    }
    if (kind === 0 && len >= 12 && u8[off + 4] === 1 && u8[off + 5] === 1) {
      var r = new Reader(u8.subarray(off + 6, off + len));
      var node = r.i32(), key = r.u16(), payload = r.p < r.u8.length ? readVal(r, r.u8_()) : null;
      out.push({ kind: 'view', off: off, len: len, node: node, event: keyName(key), payload: payload });
      return;
    }
    out.push({ kind: kind === 3 ? 'module' : 'other', off: off, len: len, id: kind === 3 ? dv.getUint16(off + 4, true) : undefined });
  }
  function encodeViewEvent(node, event, payload) {
    var w = new Writer();
    w.i32(0); w.u8_(1); w.u8_(1); w.i32(node); w.u16(keyId(event));
    writeVal(w, payload || { t: 'obj', v: [] });
    return w.out();
  }

  globalThis.PhxUI = {
    decode: decode, encode: encode,
    decodeDispatch: decodeDispatch, encodeViewEvent: encodeViewEvent,
    PROPS: PROPS, PROP_ID: PROP_ID, OPS: OPS, TAGS: TAGS
  };
})();
