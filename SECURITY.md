# Security Policy 🔒 (HYDRA-UMC-ORCHESTRATOR)

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.x.x  | ✅ Yes             |

## Reporting a Vulnerability

**CRITICAL: Do not report safety-critical vulnerabilities through public GitHub issues.**

In a distributed swarm manager, a security flaw can compromise the entire robotic fleet. If you discover a vulnerability affecting the **gRPC authentication**, **mission injection**, or **PTP clock spoofing**:

1. **Email**: Send a detailed report to `electrohobby3d@gmail.com`.
2. **Impact**: Describe if the bug allows taking unauthorized control of the fleet, bypassing centralized safety limits, or causing swarm-wide collisions.
3. **Response**: Initial acknowledgment within 48 hours.

We follow a coordinated disclosure policy to ensure hardware safety before public release.
