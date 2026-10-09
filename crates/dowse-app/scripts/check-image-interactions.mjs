import { withBrowser, evaluate, installMockRuntime } from './capture-readme-screenshots.mjs';

await withBrowser(async (client, baseUrl) => {
    await client.call('Page.addScriptToEvaluateOnNewDocument', {source: `(${installMockRuntime.toString()})('en', 'ocr');`});
    await client.call('Page.navigate', {url: baseUrl});
    await evaluate(client, `new Promise(async resolve => {
        const deadline = Date.now() + 10000;
        while (!document.querySelector('.search-input')) {
            if (Date.now() > deadline) throw new Error('Search input did not render');
            await new Promise(r => setTimeout(r,25));
        }
        const input=document.querySelector('.search-input'); input.value='Northstar'; input.dispatchEvent(new InputEvent('input',{bubbles:true}));
        await new Promise(r=>setTimeout(r,700)); resolve(true);
    })`);
    const assert = async (expression, label) => {
        for (let attempt = 0; attempt < 40; attempt++) {
            if (await evaluate(client, expression)) { console.log('PASS ' + label); return; }
            await new Promise(resolve => setTimeout(resolve, 50));
        }
        throw new Error(label);
    };
    const staysOpaque = await evaluate(client, `new Promise(resolve => {
        const listener = window.__screenshotCommands.find(x => x.args?.event === 'dowse://shown');
        if (!listener) throw new Error('Missing shown listener');
        let valid = true;
        const start = performance.now();
        window.__TAURI_INTERNALS__.runCallback(listener.args.handler, {event:'dowse://shown', id:1, payload:null});
        const sample = () => {
            const style = getComputedStyle(document.querySelector('.panel'));
            valid &&= style.opacity === '1' && style.transform === 'none';
            if (performance.now() - start < 250) requestAnimationFrame(sample);
            else resolve(valid);
        };
        sample();
    })`);
    if (!staysOpaque) throw new Error('Panel faded or scaled while showing');
    console.log('PASS shown panel stays opaque and stationary');
    await assert(`document.querySelector('.image-preview img').naturalWidth > 0`, 'normal image loaded');
    await assert(`document.querySelector('.image-preview').clientHeight > 260`, 'preview uses available height');
    await evaluate(client, `document.querySelector('.image-preview').focus(); document.querySelector('.image-preview').click()`);
    await new Promise(r=>setTimeout(r,200));
    await assert(`!!document.querySelector('dialog[open]')`, 'viewer opens');
    await assert(`document.querySelector('dialog img').getBoundingClientRect().width <= document.querySelector('.viewport').clientWidth + 1`, 'fit window');
    await evaluate(client, `Array.from(document.querySelectorAll('dialog button')).find(b=>b.textContent==='100%').click()`);
    await new Promise(r=>setTimeout(r,80));
    await assert(`document.querySelector('dialog img').getBoundingClientRect().width === 960`, 'actual size');
    await evaluate(client, `document.querySelector('[aria-label="Zoom in"]').click()`);
    await new Promise(r=>setTimeout(r,80));
    await assert(`document.querySelector('dialog img').getBoundingClientRect().width === 1200`, 'zoom in');
    await client.call('Input.dispatchMouseEvent', {type:'mousePressed', x:420,y:240,button:'left',clickCount:1});
    await client.call('Input.dispatchMouseEvent', {type:'mouseMoved', x:300,y:160,button:'left',buttons:1});
    await client.call('Input.dispatchMouseEvent', {type:'mouseReleased', x:300,y:160,button:'left',clickCount:1});
    await assert(`document.querySelector('.viewport').scrollLeft > 100`, 'drag pan');
    await client.call('Input.dispatchKeyEvent', {type:'keyDown',key:'Escape',code:'Escape',windowsVirtualKeyCode:27});
    await client.call('Input.dispatchKeyEvent', {type:'keyUp',key:'Escape',code:'Escape',windowsVirtualKeyCode:27});
    await assert(`!document.querySelector('dialog') && document.activeElement.classList.contains('image-preview')`, 'escape restores focus');
    await assert(`!window.__screenshotCommands.some(x=> (typeof x==='string'?x:JSON.stringify(x)).includes('hide_window'))`, 'escape does not hide app');
    await evaluate(client, `document.querySelector('details').open=true`);
    await new Promise(r=>setTimeout(r,80));
    await assert(`document.querySelector('.image-preview').clientHeight > 60 && document.querySelector('.image-body').scrollHeight <= document.querySelector('.image-body').clientHeight+1`, 'OCR expands without overflowing');
    // Reopen with a long screenshot; switching modes must permit scrolling to its bottom.
    await evaluate(client, `window.__TAURI_INTERNALS__.convertFileSrc = () => 'data:image/svg+xml,'+encodeURIComponent('<svg xmlns="http://www.w3.org/2000/svg" width="800" height="4000"><rect width="800" height="4000" fill="lightblue"/><text x="20" y="80" font-size="30">Long screenshot</text></svg>'); document.querySelector('.image-preview').click()`);
    await new Promise(r=>setTimeout(r,100));
    // Snapshot source remains the original image; use its DOM source to exercise the same load handler.
    await evaluate(client, `document.querySelector('dialog img').src=window.__TAURI_INTERNALS__.convertFileSrc();`);
    await new Promise(r=>setTimeout(r,100));
    await evaluate(client, `Array.from(document.querySelectorAll('dialog button')).find(b=>b.textContent==='Fit width').click()`);
    await new Promise(r=>setTimeout(r,100));
    await assert(`document.querySelector('.viewport').scrollHeight > 3000`, 'long image scrolls');
    await client.call('Emulation.setDeviceMetricsOverride',{width:640,height:420,deviceScaleFactor:1,mobile:false});
    await new Promise(r=>setTimeout(r,100));
    await assert(`Math.abs(document.querySelector('dialog img').getBoundingClientRect().width-document.querySelector('.viewport').clientWidth)<1`, 'fit width follows resize');
    await assert(`window.__screenshotErrors.length===0`, 'no browser runtime errors');
    await client.call('Input.dispatchKeyEvent', {type:'keyDown',key:'Escape',code:'Escape',windowsVirtualKeyCode:27});
    await client.call('Input.dispatchKeyEvent', {type:'keyUp',key:'Escape',code:'Escape',windowsVirtualKeyCode:27});
    const hideCount = `window.__screenshotCommands.filter(x => (typeof x==='string'?x:JSON.stringify(x)).includes('hide_window')).length`;
    await assert(`!document.querySelector('dialog') && document.activeElement.classList.contains('image-preview')`, 'viewer returns focus to preview');
    await assert(hideCount+'===0', 'closing viewer does not hide main window');
    await client.call('Input.dispatchKeyEvent', {type:'keyDown',key:'Escape',code:'Escape',windowsVirtualKeyCode:27});
    await client.call('Input.dispatchKeyEvent', {type:'keyUp',key:'Escape',code:'Escape',windowsVirtualKeyCode:27});
    await assert(hideCount+'===1', 'Escape on restored preview focus hides window exactly once');
    await evaluate(client, `document.querySelector('.search-input').focus()`);
    await client.call('Input.dispatchKeyEvent', {type:'keyDown',key:'Escape',code:'Escape',windowsVirtualKeyCode:27});
    await client.call('Input.dispatchKeyEvent', {type:'keyUp',key:'Escape',code:'Escape',windowsVirtualKeyCode:27});
    await assert(hideCount+'===2', 'Escape in search input hides window exactly once');
    await evaluate(client, `document.querySelector('.search-input').dispatchEvent(new KeyboardEvent('keydown',{key:',',code:'Comma',ctrlKey:true,bubbles:true}))`);
    await new Promise(r=>setTimeout(r,100));
    await client.call('Input.dispatchKeyEvent', {type:'keyDown',key:'Escape',code:'Escape',windowsVirtualKeyCode:27});
    await client.call('Input.dispatchKeyEvent', {type:'keyUp',key:'Escape',code:'Escape',windowsVirtualKeyCode:27});
    await assert(hideCount+'===2 && !document.querySelector(".scrim")', 'settings consumes Escape before window hide');

});
