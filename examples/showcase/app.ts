import { Column, Row, Text, Button } from "@w3cos/std"

export default Column({
  style: { background: "#0a0a12", gap: 0 },
  children: [
    Row({
      style: { padding: 16, background: "#12121f", justifyContent: "spaceBetween", alignItems: "center" },
      children: [
        Row({
          style: { gap: 10, alignItems: "center" },
          children: [
            Text("◆", { style: { fontSize: 22, color: "#6c5ce7" } }),
            Text("W3C OS", { style: { fontSize: 18, color: "#dfe6e9", fontWeight: 700 } }),
          ]
        }),
        Row({
          style: { gap: 20, alignItems: "center" },
          children: [
            Text("Dashboard", { style: { fontSize: 14, color: "#a0a0c0" } }),
            Text("Apps", { style: { fontSize: 14, color: "#a0a0c0" } }),
            Text("Settings", { style: { fontSize: 14, color: "#a0a0c0" } }),
            Text("●", { style: { fontSize: 10, color: "#00b894" } }),
          ]
        }),
      ]
    }),

    Row({
      style: { gap: 0, flexGrow: 1 },
      children: [
        Column({
          style: { width: "220", padding: 20, gap: 4, background: "#10101c" },
          children: [
            Text("NAVIGATION", { style: { fontSize: 11, color: "#505070", fontWeight: 700 } }),
            Row({
              style: { padding: 10, borderRadius: 8, background: "#6c5ce7", alignItems: "center", gap: 8 },
              children: [
                Text("⬡", { style: { fontSize: 14, color: "#ffffff" } }),
                Text("Overview", { style: { fontSize: 14, color: "#ffffff", fontWeight: 600 } }),
              ]
            }),
            Row({
              style: { padding: 10, borderRadius: 8, alignItems: "center", gap: 8 },
              children: [
                Text("◈", { style: { fontSize: 14, color: "#606080" } }),
                Text("Applications", { style: { fontSize: 14, color: "#a0a0c0" } }),
              ]
            }),
            Row({
              style: { padding: 10, borderRadius: 8, alignItems: "center", gap: 8 },
              children: [
                Text("◉", { style: { fontSize: 14, color: "#606080" } }),
                Text("Processes", { style: { fontSize: 14, color: "#a0a0c0" } }),
              ]
            }),
            Row({
              style: { padding: 10, borderRadius: 8, alignItems: "center", gap: 8 },
              children: [
                Text("⬢", { style: { fontSize: 14, color: "#606080" } }),
                Text("File System", { style: { fontSize: 14, color: "#a0a0c0" } }),
              ]
            }),
            Row({
              style: { padding: 10, borderRadius: 8, alignItems: "center", gap: 8 },
              children: [
                Text("◎", { style: { fontSize: 14, color: "#606080" } }),
                Text("Network", { style: { fontSize: 14, color: "#a0a0c0" } }),
              ]
            }),
            Row({
              style: { padding: 10, borderRadius: 8, alignItems: "center", gap: 8 },
              children: [
                Text("⚙", { style: { fontSize: 14, color: "#606080" } }),
                Text("AI Agents", { style: { fontSize: 14, color: "#a0a0c0" } }),
              ]
            }),
          ]
        }),

        Column({
          style: { flexGrow: 1, padding: 24, gap: 20 },
          children: [
            Row({
              style: { justifyContent: "spaceBetween", alignItems: "center" },
              children: [
                Column({
                  style: { gap: 4 },
                  children: [
                    Text("System Overview", { style: { fontSize: 24, color: "#f0f0ff", fontWeight: 700 } }),
                    Text("All systems operational", { style: { fontSize: 14, color: "#00b894" } }),
                  ]
                }),
                Button("New App", { style: { background: "#6c5ce7", color: "#ffffff", borderRadius: 8, fontSize: 14 } }),
              ]
            }),

            Row({
              style: { gap: 16 },
              children: [
                Column({
                  style: { flexGrow: 1, padding: 20, background: "#16162a", borderRadius: 12, gap: 8 },
                  children: [
                    Row({
                      style: { justifyContent: "spaceBetween", alignItems: "center" },
                      children: [
                        Text("CPU", { style: { fontSize: 13, color: "#8080a0" } }),
                        Text("23%", { style: { fontSize: 13, color: "#00b894" } }),
                      ]
                    }),
                    Text("1.2 GHz", { style: { fontSize: 28, color: "#f0f0ff", fontWeight: 700 } }),
                    Row({
                      style: { height: "6", borderRadius: 3, background: "#1e1e38" },
                      children: [
                        Column({ style: { width: "23%", height: "6", borderRadius: 3, background: "#00b894" }, children: [] }),
                      ]
                    }),
                  ]
                }),
                Column({
                  style: { flexGrow: 1, padding: 20, background: "#16162a", borderRadius: 12, gap: 8 },
                  children: [
                    Row({
                      style: { justifyContent: "spaceBetween", alignItems: "center" },
                      children: [
                        Text("Memory", { style: { fontSize: 13, color: "#8080a0" } }),
                        Text("67%", { style: { fontSize: 13, color: "#fdcb6e" } }),
                      ]
                    }),
                    Text("5.4 / 8 GB", { style: { fontSize: 28, color: "#f0f0ff", fontWeight: 700 } }),
                    Row({
                      style: { height: "6", borderRadius: 3, background: "#1e1e38" },
                      children: [
                        Column({ style: { width: "67%", height: "6", borderRadius: 3, background: "#fdcb6e" }, children: [] }),
                      ]
                    }),
                  ]
                }),
                Column({
                  style: { flexGrow: 1, padding: 20, background: "#16162a", borderRadius: 12, gap: 8 },
                  children: [
                    Row({
                      style: { justifyContent: "spaceBetween", alignItems: "center" },
                      children: [
                        Text("Storage", { style: { fontSize: 13, color: "#8080a0" } }),
                        Text("41%", { style: { fontSize: 13, color: "#74b9ff" } }),
                      ]
                    }),
                    Text("205 / 512 GB", { style: { fontSize: 28, color: "#f0f0ff", fontWeight: 700 } }),
                    Row({
                      style: { height: "6", borderRadius: 3, background: "#1e1e38" },
                      children: [
                        Column({ style: { width: "41%", height: "6", borderRadius: 3, background: "#74b9ff" }, children: [] }),
                      ]
                    }),
                  ]
                }),
                Column({
                  style: { flexGrow: 1, padding: 20, background: "#16162a", borderRadius: 12, gap: 8 },
                  children: [
                    Row({
                      style: { justifyContent: "spaceBetween", alignItems: "center" },
                      children: [
                        Text("Network", { style: { fontSize: 13, color: "#8080a0" } }),
                        Text("↑↓", { style: { fontSize: 13, color: "#a29bfe" } }),
                      ]
                    }),
                    Text("84 Mbps", { style: { fontSize: 28, color: "#f0f0ff", fontWeight: 700 } }),
                    Text("12ms latency", { style: { fontSize: 13, color: "#606080" } }),
                  ]
                }),
              ]
            }),

            Row({
              style: { gap: 16, flexGrow: 1 },
              children: [
                Column({
                  style: { flexGrow: 2, padding: 20, background: "#16162a", borderRadius: 12, gap: 12 },
                  children: [
                    Text("Running Applications", { style: { fontSize: 16, color: "#f0f0ff", fontWeight: 700 } }),
                    Row({
                      style: { padding: 12, background: "#1c1c34", borderRadius: 8, justifyContent: "spaceBetween", alignItems: "center" },
                      children: [
                        Row({
                          style: { gap: 10, alignItems: "center" },
                          children: [
                            Text("◆", { style: { fontSize: 12, color: "#6c5ce7" } }),
                            Text("file-manager.w3c", { style: { fontSize: 14, color: "#d0d0e0" } }),
                          ]
                        }),
                        Row({
                          style: { gap: 16, alignItems: "center" },
                          children: [
                            Text("24 MB", { style: { fontSize: 12, color: "#8080a0" } }),
                            Text("2%", { style: { fontSize: 12, color: "#00b894" } }),
                            Text("PID 1024", { style: { fontSize: 12, color: "#606080" } }),
                          ]
                        }),
                      ]
                    }),
                    Row({
                      style: { padding: 12, background: "#1c1c34", borderRadius: 8, justifyContent: "spaceBetween", alignItems: "center" },
                      children: [
                        Row({
                          style: { gap: 10, alignItems: "center" },
                          children: [
                            Text("◆", { style: { fontSize: 12, color: "#00b894" } }),
                            Text("terminal.w3c", { style: { fontSize: 14, color: "#d0d0e0" } }),
                          ]
                        }),
                        Row({
                          style: { gap: 16, alignItems: "center" },
                          children: [
                            Text("18 MB", { style: { fontSize: 12, color: "#8080a0" } }),
                            Text("1%", { style: { fontSize: 12, color: "#00b894" } }),
                            Text("PID 1025", { style: { fontSize: 12, color: "#606080" } }),
                          ]
                        }),
                      ]
                    }),
                    Row({
                      style: { padding: 12, background: "#1c1c34", borderRadius: 8, justifyContent: "spaceBetween", alignItems: "center" },
                      children: [
                        Row({
                          style: { gap: 10, alignItems: "center" },
                          children: [
                            Text("◆", { style: { fontSize: 12, color: "#fdcb6e" } }),
                            Text("ai-agent-hub.w3c", { style: { fontSize: 14, color: "#d0d0e0" } }),
                          ]
                        }),
                        Row({
                          style: { gap: 16, alignItems: "center" },
                          children: [
                            Text("156 MB", { style: { fontSize: 12, color: "#8080a0" } }),
                            Text("15%", { style: { fontSize: 12, color: "#fdcb6e" } }),
                            Text("PID 1026", { style: { fontSize: 12, color: "#606080" } }),
                          ]
                        }),
                      ]
                    }),
                    Row({
                      style: { padding: 12, background: "#1c1c34", borderRadius: 8, justifyContent: "spaceBetween", alignItems: "center" },
                      children: [
                        Row({
                          style: { gap: 10, alignItems: "center" },
                          children: [
                            Text("◆", { style: { fontSize: 12, color: "#74b9ff" } }),
                            Text("code-editor.w3c", { style: { fontSize: 14, color: "#d0d0e0" } }),
                          ]
                        }),
                        Row({
                          style: { gap: 16, alignItems: "center" },
                          children: [
                            Text("42 MB", { style: { fontSize: 12, color: "#8080a0" } }),
                            Text("5%", { style: { fontSize: 12, color: "#00b894" } }),
                            Text("PID 1027", { style: { fontSize: 12, color: "#606080" } }),
                          ]
                        }),
                      ]
                    }),
                  ]
                }),

                Column({
                  style: { flexGrow: 1, padding: 20, background: "#16162a", borderRadius: 12, gap: 12 },
                  children: [
                    Text("AI Agent Status", { style: { fontSize: 16, color: "#f0f0ff", fontWeight: 700 } }),
                    Column({
                      style: { padding: 14, background: "#1c1c34", borderRadius: 8, gap: 6 },
                      children: [
                        Row({
                          style: { justifyContent: "spaceBetween" },
                          children: [
                            Text("Code Agent", { style: { fontSize: 13, color: "#d0d0e0" } }),
                            Text("Active", { style: { fontSize: 12, color: "#00b894" } }),
                          ]
                        }),
                        Text("Building filesystem module...", { style: { fontSize: 12, color: "#606080" } }),
                      ]
                    }),
                    Column({
                      style: { padding: 14, background: "#1c1c34", borderRadius: 8, gap: 6 },
                      children: [
                        Row({
                          style: { justifyContent: "spaceBetween" },
                          children: [
                            Text("Review Agent", { style: { fontSize: 13, color: "#d0d0e0" } }),
                            Text("Idle", { style: { fontSize: 12, color: "#8080a0" } }),
                          ]
                        }),
                        Text("Waiting for PR #42", { style: { fontSize: 12, color: "#606080" } }),
                      ]
                    }),
                    Column({
                      style: { padding: 14, background: "#1c1c34", borderRadius: 8, gap: 6 },
                      children: [
                        Row({
                          style: { justifyContent: "spaceBetween" },
                          children: [
                            Text("Test Agent", { style: { fontSize: 13, color: "#d0d0e0" } }),
                            Text("Running", { style: { fontSize: 12, color: "#fdcb6e" } }),
                          ]
                        }),
                        Text("47/52 tests passed", { style: { fontSize: 12, color: "#606080" } }),
                      ]
                    }),
                    Column({
                      style: { padding: 14, background: "#1c1c34", borderRadius: 8, gap: 6 },
                      children: [
                        Row({
                          style: { justifyContent: "spaceBetween" },
                          children: [
                            Text("Security Agent", { style: { fontSize: 13, color: "#d0d0e0" } }),
                            Text("Active", { style: { fontSize: 12, color: "#00b894" } }),
                          ]
                        }),
                        Text("Scanning PR #41...", { style: { fontSize: 12, color: "#606080" } }),
                      ]
                    }),
                  ]
                }),
              ]
            }),
          ]
        }),
      ]
    }),
  ]
})
