/**
 * Unicode Demo Plugin for Red Editor
 * 
 * This plugin demonstrates proper handling of multi-byte characters including:
 * - Emoji (👋, 👨‍👩‍👧‍👦)
 * - CJK characters (你好, 世界)
 * - Combining characters (café)
 * 
 * It showcases the difference between character positions and display columns.
 */

export function activate(red) {
    // Add a command to demonstrate Unicode handling
    red.addCommand('unicode:demo', async () => {
        await demonstrateUnicodeHandling(red);
    });

    // Add a command to show cursor info with Unicode context
    red.addCommand('unicode:cursor-info', async () => {
        await showCursorInfo(red);
    });

    // Add a command to insert various Unicode characters
    red.addCommand('unicode:insert-samples', async () => {
        await insertUnicodeSamples(red);
    });

    // Add a command to test new Unicode helper methods
    red.addCommand('unicode:test-helpers', async () => {
        await testUnicodeHelpers(red);
    });

    red.log('Unicode demo plugin loaded! Commands: unicode:demo, unicode:cursor-info, unicode:insert-samples, unicode:test-helpers');
}

/**
 * Main demo showing various Unicode operations
 */
async function demonstrateUnicodeHandling(red) {
    const pos = await red.getCursorPosition();
    
    // Insert a line with mixed Unicode content
    const demoText = 'Hello 👋 世界! Café ☕ Family:👨‍👩‍👧‍👦';
    red.insertText(pos.x, pos.y, demoText);
    
    // Move cursor through the text to show character vs display column differences
    red.log('=== Unicode Demo ===');
    red.log(`Inserted: "${demoText}"`);
    
    // Demonstrate cursor positioning
    await demonstrateCursorMovement(red, pos.y);
}

/**
 * Show detailed cursor information including display column
 */
async function showCursorInfo(red) {
    const pos = await red.getCursorPosition();
    const displayCol = await red.getCursorDisplayColumn();
    
    // Get the current line to show context
    const text = await red.getBufferText(pos.y, pos.y + 1);
    const line = text.trimEnd();
    
    red.log('=== Cursor Information ===');
    red.log(`Line ${pos.y}: "${line}"`);
    red.log(`Character position: ${pos.x}`);
    red.log(`Display column: ${displayCol}`);
    
    // Show what character is at cursor
    if (pos.x < line.length) {
        const char = line[pos.x];
        const charCode = char.charCodeAt(0);
        red.log(`Character at cursor: "${char}" (U+${charCode.toString(16).toUpperCase().padStart(4, '0')})`);
        
        // Check if it's a wide character
        if (isWideCharacter(char)) {
            red.log('This is a wide character (takes 2 display columns)');
        }
    }
}

/**
 * Insert various Unicode samples at cursor position
 */
async function insertUnicodeSamples(red) {
    const samples = [
        { name: 'Basic Emoji', text: '😀😃😄😁' },
        { name: 'CJK Text', text: '你好世界 (Hello World)' },
        { name: 'Complex Emoji', text: '👨‍👩‍👧‍👦 (Family)' },
        { name: 'Flags', text: '🇺🇸🇯🇵🇬🇧🇫🇷' },
        { name: 'Mixed Script', text: 'Hello नमस्ते 你好 مرحبا' },
        { name: 'Math Symbols', text: '∑∏∫√∞≈≠' },
        { name: 'Combining Marks', text: 'café naïve résumé' }
    ];
    
    // Show picker to select which sample to insert
    const sampleNames = samples.map(s => s.name);
    const selected = await red.pick('Select Unicode sample to insert:', sampleNames);
    
    if (selected) {
        const sample = samples.find(s => s.name === selected);
        const pos = await red.getCursorPosition();
        red.insertText(pos.x, pos.y, sample.text);
        red.log(`Inserted ${sample.name}: ${sample.text}`);
    }
}

/**
 * Demonstrate cursor movement through Unicode text
 */
async function demonstrateCursorMovement(red, line) {
    // Array of interesting positions to check
    const positions = [
        { char: 0, desc: 'Start of line' },
        { char: 5, desc: 'Before space' },
        { char: 6, desc: 'Before emoji 👋' },
        { char: 7, desc: 'After emoji 👋' },
        { char: 8, desc: 'Before space' },
        { char: 9, desc: 'Before 世' },
        { char: 10, desc: 'Before 界' },
        { char: 11, desc: 'After 界' }
    ];
    
    red.log('\n--- Cursor Movement Demo ---');
    
    for (const pos of positions) {
        red.setCursorPosition(pos.char, line);
        const displayCol = await red.getCursorDisplayColumn();
        red.log(`Char ${pos.char} (${pos.desc}): Display column ${displayCol}`);
    }
}

/**
 * Helper function to check if a character is wide (takes 2 columns)
 */
function isWideCharacter(char) {
    const code = char.charCodeAt(0);
    
    // CJK ranges
    if (code >= 0x1100 && code <= 0x115F) return true; // Hangul Jamo
    if (code >= 0x2E80 && code <= 0x9FFF) return true; // CJK
    if (code >= 0xAC00 && code <= 0xD7AF) return true; // Hangul Syllables
    if (code >= 0xF900 && code <= 0xFAFF) return true; // CJK Compatibility
    
    // Full-width forms
    if (code >= 0xFF00 && code <= 0xFF60) return true;
    if (code >= 0xFFE0 && code <= 0xFFE6) return true;
    
    // Many emoji are wide (simplified check)
    if (code >= 0x1F300 && code <= 0x1F9FF) return true;
    
    return false;
}

/**
 * Test new Unicode helper methods
 */
async function testUnicodeHelpers(red) {
    red.log('=== Testing Unicode Helper Methods ===');
    
    // Test getTextDisplayWidth
    const testStrings = [
        { text: 'Hello', expected: 5 },
        { text: '你好', expected: 4 },
        { text: '👋', expected: 2 },
        { text: 'café', expected: 4 },
        { text: 'A👨‍👩‍👧‍👦B', expected: 4 } // A + emoji (2) + B
    ];
    
    red.log('\n--- Testing getTextDisplayWidth ---');
    for (const test of testStrings) {
        const width = await red.getTextDisplayWidth(test.text);
        red.log(`"${test.text}" has display width: ${width} (expected: ${test.expected})`);
    }
    
    // Test coordinate conversions
    const pos = await red.getCursorPosition();
    const line = await red.getBufferText(pos.y, pos.y + 1);
    
    red.log('\n--- Testing coordinate conversions ---');
    red.log(`Current line: "${line.trimEnd()}"`);
    
    // Test char index to display column
    for (let i = 0; i <= Math.min(10, line.length); i++) {
        const displayCol = await red.charIndexToDisplayColumn(i, pos.y);
        red.log(`Char index ${i} => Display column ${displayCol}`);
    }
    
    // Test display column to char index
    red.log('\n--- Testing display column to char index ---');
    for (let col = 0; col <= 10; col++) {
        const charIndex = await red.displayColumnToCharIndex(col, pos.y);
        red.log(`Display column ${col} => Char index ${charIndex}`);
    }
}

/**
 * Plugin metadata
 */
export const metadata = {
    name: 'unicode-demo',
    version: '1.0.0',
    description: 'Demonstrates proper handling of multi-byte Unicode characters',
    author: 'Red Editor Team',
    keywords: ['unicode', 'emoji', 'cjk', 'demo'],
    main: 'unicode_demo.js',
    capabilities: {
        commands: true,
        events: false,
        buffer_manipulation: true,
        ui_components: false,
        lsp_integration: false
    }
};