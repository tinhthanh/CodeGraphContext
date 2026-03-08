function testProjectParser() {
    const output = `
┏━━━━━━━━━━━━━━━━━━┳━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┳━━━━━━━━━┓
┃ Name             ┃ Path                                    ┃ Type    ┃
┡━━━━━━━━━━━━━━━━━━╇━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╇━━━━━━━━━┩
│ CodeGraphContext │ /home/shashank/Desktop/CodeGraphContext │ Project │
└──────────────────┴─────────────────────────────────────────┴─────────┘
`;

    const projects = [];

    // Remove header and top border - look for the separator line
    const contentStartIndex = output.search(/[┡├]/);
    if (contentStartIndex === -1) {
        console.log("No table found");
        return;
    }

    // Get only the content part
    const contentPart = output.substring(contentStartIndex);

    // Split into rows
    // Matches ┡━...━┩ or ├─...─┤ or bottom └─...─┘ or ╰─...─╯
    const rowBlocks = contentPart.split(/\n\s*[┡├└╰][━─┼┴]+\s*[┩┤┘╯]/);
    console.log(`Found ${rowBlocks.length} blocks`);

    for (const block of rowBlocks) {
        // console.log("Block:", JSON.stringify(block));
        const lines = block.split('\n').filter(l => l.includes('│') || l.includes('┃'));
        if (lines.length === 0) continue;

        let name = '';
        let path = '';
        let type = '';

        for (const line of lines) {
            // Determine separator char (│ or ┃)
            const sep = line.includes('┃') ? '┃' : '│';
            const parts = line.split(sep);

            if (parts.length >= 4) {
                name += parts[1].trim();
                path += parts[2].trim();
                type += parts[3].trim();
            }
        }

        if (name && path) {
            projects.push({
                name: name,
                path: path,
                type: type || 'Project'
            });
        }
    }

    console.log(JSON.stringify(projects, null, 2));
}

testProjectParser();
