const fs = require('fs');

async function testParser() {
    const rawOutput = `
Function 'handle_tool_call' calls:
╭──────────────────────────────────────┬──────────────────────────────────────────────────────────────────────────┬────────────╮
│ Called Function                      │ Location                                                                 │ Type       │
├──────────────────────────────────────┼──────────────────────────────────────────────────────────────────────────┼────────────┤
│ add_code_to_graph_tool               │ /home/shashank/Desktop/CodeGraphContext/src/codegraphcontext/server.py:1 │ 📝 Project │
│                                      │ 40                                                                       │            │
├──────────────────────────────────────┼──────────────────────────────────────────────────────────────────────────┼────────────┤
│ add_package_to_graph_tool            │ /home/shashank/Desktop/CodeGraphContext/src/codegraphcontext/server.py:1 │ 📝 Project │
│                                      │ 49                                                                       │            │
╰──────────────────────────────────────┴──────────────────────────────────────────────────────────────────────────┴────────────╯
`;

    console.log("Input:");
    console.log(rawOutput);

    const results = [];

    // Remove header and top border
    const contentStartIndex = rawOutput.indexOf('├');
    if (contentStartIndex === -1) {
        console.log("No table found");
        return;
    }

    // Get only the content part (after the first separator)
    const contentPart = rawOutput.substring(contentStartIndex);

    // Split into "blocks" separated by the row separator line '├─...─┤' or bottom '╰─...─╯'
    const rowBlocks = contentPart.split(/\n\s*[├╰][─┼┴]+\s*[┤╯]/);

    for (const block of rowBlocks) {
        const lines = block.split('\n').filter(l => l.includes('│'));
        if (lines.length === 0) continue;

        // Extract content from each column across all lines in this block
        let name = '';
        let location = '';

        for (const line of lines) {
            const parts = line.split('│');
            if (parts.length >= 3) {
                name += parts[1].trim();
                location += parts[2].trim();
            }
        }

        if (name && location) {
            console.log(`Found: Name="${name}", Loc="${location}"`);
            const locationMatch = location.match(/^(.+?):(\d+)$/);
            if (locationMatch) {
                results.push({
                    name: name,
                    file: locationMatch[1],
                    line: parseInt(locationMatch[2])
                });
            }
        }
    }

    console.log("\nParsed Results:");
    console.log(JSON.stringify(results, null, 2));
}

testParser();
