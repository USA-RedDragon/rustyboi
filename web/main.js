import init, * as wasm from './web-pack/rustyboi_platform_lib.js';

let emulatorInitialized = false;

// Canvas sizing will be handled by the Rust/WASM code

async function initializeEmulator() {
    try {
        console.log('Initializing RustyBoi WASM module...');
        // Initialize the WASM module
        await init();
        console.log('WASM module loaded successfully');
        
        // The emulator should start automatically due to the wasm_bindgen(start) attribute
        console.log('RustyBoi emulator should be starting automatically...');
        
        emulatorInitialized = true;
        
        console.log('RustyBoi emulator started successfully!');
        
    } catch (error) {
        console.error('Failed to initialize emulator:', error);
    }
}

// Initialize everything when the page loads
window.addEventListener('DOMContentLoaded', () => {
    console.log('DOM loaded, setting up RustyBoi...');
    
    // Start initializing the emulator immediately
    initializeEmulator().catch(error => {
        console.error('Failed to start emulator:', error);
    });
});

// Handle page visibility changes to pause/resume emulator
document.addEventListener('visibilitychange', () => {
    if (emulatorInitialized) {
        if (document.hidden) {
            console.log('Page hidden, emulator should pause');
            // You might want to expose a pause function from Rust
        } else {
            console.log('Page visible, emulator should resume');
            // You might want to expose a resume function from Rust
        }
    }
});

// Prevent context menu on right-click (common for games)
document.addEventListener('contextmenu', (event) => {
    event.preventDefault();
});

console.log('RustyBoi JavaScript loader initialized');
