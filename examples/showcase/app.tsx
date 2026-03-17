import { Column, Row, Text, Button } from "@w3cos/std"

export default
<Column style={{ background: "#0a0a12", gap: 0 }}>
  <Row style={{ padding: 16, background: "#12121f", justifyContent: "spaceBetween", alignItems: "center" }}>
    <Row style={{ gap: 10, alignItems: "center" }}>
      <Text style={{ fontSize: 22, color: "#6c5ce7" }}>◆</Text>
      <Text style={{ fontSize: 18, color: "#dfe6e9", fontWeight: 700 }}>W3C OS</Text>
    </Row>
    <Row style={{ gap: 20, alignItems: "center" }}>
      <Text style={{ fontSize: 14, color: "#a0a0c0" }}>Dashboard</Text>
      <Text style={{ fontSize: 14, color: "#a0a0c0" }}>Apps</Text>
      <Text style={{ fontSize: 14, color: "#a0a0c0" }}>Settings</Text>
    </Row>
  </Row>

  <Row style={{ gap: 0, flexGrow: 1 }}>
    <Column style={{ width: "220", padding: 20, gap: 4, background: "#10101c" }}>
      <Text style={{ fontSize: 11, color: "#505070", fontWeight: 700 }}>NAVIGATION</Text>
      <Row style={{ padding: 10, borderRadius: 8, background: "#6c5ce7", alignItems: "center", gap: 8 }}>
        <Text style={{ fontSize: 14, color: "#ffffff" }}>⬡</Text>
        <Text style={{ fontSize: 14, color: "#ffffff", fontWeight: 600 }}>Overview</Text>
      </Row>
      <Row style={{ padding: 10, borderRadius: 8, alignItems: "center", gap: 8 }}>
        <Text style={{ fontSize: 14, color: "#606080" }}>◈</Text>
        <Text style={{ fontSize: 14, color: "#a0a0c0" }}>Applications</Text>
      </Row>
      <Row style={{ padding: 10, borderRadius: 8, alignItems: "center", gap: 8 }}>
        <Text style={{ fontSize: 14, color: "#606080" }}>◉</Text>
        <Text style={{ fontSize: 14, color: "#a0a0c0" }}>Processes</Text>
      </Row>
      <Row style={{ padding: 10, borderRadius: 8, alignItems: "center", gap: 8 }}>
        <Text style={{ fontSize: 14, color: "#606080" }}>⬢</Text>
        <Text style={{ fontSize: 14, color: "#a0a0c0" }}>File System</Text>
      </Row>
      <Row style={{ padding: 10, borderRadius: 8, alignItems: "center", gap: 8 }}>
        <Text style={{ fontSize: 14, color: "#606080" }}>◎</Text>
        <Text style={{ fontSize: 14, color: "#a0a0c0" }}>Network</Text>
      </Row>
      <Row style={{ padding: 10, borderRadius: 8, alignItems: "center", gap: 8 }}>
        <Text style={{ fontSize: 14, color: "#606080" }}>⚙</Text>
        <Text style={{ fontSize: 14, color: "#a0a0c0" }}>AI Agents</Text>
      </Row>
    </Column>

    <Column style={{ flexGrow: 1, padding: 24, gap: 20 }}>
      <Row style={{ justifyContent: "spaceBetween", alignItems: "center" }}>
        <Column style={{ gap: 4 }}>
          <Text style={{ fontSize: 24, color: "#f0f0ff", fontWeight: 700 }}>System Overview</Text>
          <Text style={{ fontSize: 14, color: "#00b894" }}>All systems operational</Text>
        </Column>
        <Button style={{ background: "#6c5ce7", color: "#ffffff", borderRadius: 8, fontSize: 14 }}>New App</Button>
      </Row>

      <Row style={{ gap: 16 }}>
        <Column style={{ flexGrow: 1, padding: 20, background: "#16162a", borderRadius: 12, gap: 8 }}>
          <Row style={{ justifyContent: "spaceBetween", alignItems: "center" }}>
            <Text style={{ fontSize: 13, color: "#8080a0" }}>CPU</Text>
            <Text style={{ fontSize: 13, color: "#00b894" }}>23%</Text>
          </Row>
          <Text style={{ fontSize: 28, color: "#f0f0ff", fontWeight: 700 }}>1.2 GHz</Text>
          <Row style={{ height: "6", borderRadius: 3, background: "#1e1e38" }}>
            <Column style={{ width: "23%", height: "6", borderRadius: 3, background: "#00b894" }} />
          </Row>
        </Column>
        <Column style={{ flexGrow: 1, padding: 20, background: "#16162a", borderRadius: 12, gap: 8 }}>
          <Row style={{ justifyContent: "spaceBetween", alignItems: "center" }}>
            <Text style={{ fontSize: 13, color: "#8080a0" }}>Memory</Text>
            <Text style={{ fontSize: 13, color: "#fdcb6e" }}>67%</Text>
          </Row>
          <Text style={{ fontSize: 28, color: "#f0f0ff", fontWeight: 700 }}>5.4 / 8 GB</Text>
          <Row style={{ height: "6", borderRadius: 3, background: "#1e1e38" }}>
            <Column style={{ width: "67%", height: "6", borderRadius: 3, background: "#fdcb6e" }} />
          </Row>
        </Column>
        <Column style={{ flexGrow: 1, padding: 20, background: "#16162a", borderRadius: 12, gap: 8 }}>
          <Row style={{ justifyContent: "spaceBetween", alignItems: "center" }}>
            <Text style={{ fontSize: 13, color: "#8080a0" }}>Storage</Text>
            <Text style={{ fontSize: 13, color: "#74b9ff" }}>41%</Text>
          </Row>
          <Text style={{ fontSize: 28, color: "#f0f0ff", fontWeight: 700 }}>205 / 512 GB</Text>
          <Row style={{ height: "6", borderRadius: 3, background: "#1e1e38" }}>
            <Column style={{ width: "41%", height: "6", borderRadius: 3, background: "#74b9ff" }} />
          </Row>
        </Column>
        <Column style={{ flexGrow: 1, padding: 20, background: "#16162a", borderRadius: 12, gap: 8 }}>
          <Row style={{ justifyContent: "spaceBetween", alignItems: "center" }}>
            <Text style={{ fontSize: 13, color: "#8080a0" }}>Network</Text>
            <Text style={{ fontSize: 13, color: "#a29bfe" }}>↑↓</Text>
          </Row>
          <Text style={{ fontSize: 28, color: "#f0f0ff", fontWeight: 700 }}>84 Mbps</Text>
          <Text style={{ fontSize: 13, color: "#606080" }}>12ms latency</Text>
        </Column>
      </Row>

      <Row style={{ gap: 16, flexGrow: 1 }}>
        <Column style={{ flexGrow: 2, padding: 20, background: "#16162a", borderRadius: 12, gap: 12 }}>
          <Text style={{ fontSize: 16, color: "#f0f0ff", fontWeight: 700 }}>Running Applications</Text>
          <Row style={{ padding: 12, background: "#1c1c34", borderRadius: 8, justifyContent: "spaceBetween", alignItems: "center" }}>
            <Row style={{ gap: 10, alignItems: "center" }}>
              <Text style={{ fontSize: 12, color: "#6c5ce7" }}>◆</Text>
              <Text style={{ fontSize: 14, color: "#d0d0e0" }}>file-manager.w3c</Text>
            </Row>
            <Row style={{ gap: 16, alignItems: "center" }}>
              <Text style={{ fontSize: 12, color: "#8080a0" }}>24 MB</Text>
              <Text style={{ fontSize: 12, color: "#00b894" }}>2%</Text>
              <Text style={{ fontSize: 12, color: "#606080" }}>PID 1024</Text>
            </Row>
          </Row>
          <Row style={{ padding: 12, background: "#1c1c34", borderRadius: 8, justifyContent: "spaceBetween", alignItems: "center" }}>
            <Row style={{ gap: 10, alignItems: "center" }}>
              <Text style={{ fontSize: 12, color: "#00b894" }}>◆</Text>
              <Text style={{ fontSize: 14, color: "#d0d0e0" }}>terminal.w3c</Text>
            </Row>
            <Row style={{ gap: 16, alignItems: "center" }}>
              <Text style={{ fontSize: 12, color: "#8080a0" }}>18 MB</Text>
              <Text style={{ fontSize: 12, color: "#00b894" }}>1%</Text>
              <Text style={{ fontSize: 12, color: "#606080" }}>PID 1025</Text>
            </Row>
          </Row>
          <Row style={{ padding: 12, background: "#1c1c34", borderRadius: 8, justifyContent: "spaceBetween", alignItems: "center" }}>
            <Row style={{ gap: 10, alignItems: "center" }}>
              <Text style={{ fontSize: 12, color: "#fdcb6e" }}>◆</Text>
              <Text style={{ fontSize: 14, color: "#d0d0e0" }}>ai-agent-hub.w3c</Text>
            </Row>
            <Row style={{ gap: 16, alignItems: "center" }}>
              <Text style={{ fontSize: 12, color: "#8080a0" }}>156 MB</Text>
              <Text style={{ fontSize: 12, color: "#fdcb6e" }}>15%</Text>
              <Text style={{ fontSize: 12, color: "#606080" }}>PID 1026</Text>
            </Row>
          </Row>
        </Column>

        <Column style={{ flexGrow: 1, padding: 20, background: "#16162a", borderRadius: 12, gap: 12 }}>
          <Text style={{ fontSize: 16, color: "#f0f0ff", fontWeight: 700 }}>AI Agent Status</Text>
          <Column style={{ padding: 14, background: "#1c1c34", borderRadius: 8, gap: 6 }}>
            <Row style={{ justifyContent: "spaceBetween" }}>
              <Text style={{ fontSize: 13, color: "#d0d0e0" }}>Code Agent</Text>
              <Text style={{ fontSize: 12, color: "#00b894" }}>Active</Text>
            </Row>
            <Text style={{ fontSize: 12, color: "#606080" }}>Building filesystem module...</Text>
          </Column>
          <Column style={{ padding: 14, background: "#1c1c34", borderRadius: 8, gap: 6 }}>
            <Row style={{ justifyContent: "spaceBetween" }}>
              <Text style={{ fontSize: 13, color: "#d0d0e0" }}>Review Agent</Text>
              <Text style={{ fontSize: 12, color: "#8080a0" }}>Idle</Text>
            </Row>
            <Text style={{ fontSize: 12, color: "#606080" }}>Waiting for PR #42</Text>
          </Column>
          <Column style={{ padding: 14, background: "#1c1c34", borderRadius: 8, gap: 6 }}>
            <Row style={{ justifyContent: "spaceBetween" }}>
              <Text style={{ fontSize: 13, color: "#d0d0e0" }}>Test Agent</Text>
              <Text style={{ fontSize: 12, color: "#fdcb6e" }}>Running</Text>
            </Row>
            <Text style={{ fontSize: 12, color: "#606080" }}>47/52 tests passed</Text>
          </Column>
        </Column>
      </Row>
    </Column>
  </Row>
</Column>
