variable "REGISTRY" { default = "ghcr.io/accelero/agent-platform" }
variable "IMAGE_TAG" { default = "dev" }
variable "SOURCE_REVISION" { default = "" }

function "image" {
  params = [name]
  result = "${REGISTRY}/${name}:${IMAGE_TAG}"
}

group "default" {
  targets = ["hermes", "execution-manager", "worker", "multica-backend", "state"]
}
target "metadata" {
  labels = {
    "org.opencontainers.image.source" = "https://github.com/Accelero/agent-platform"
    "org.opencontainers.image.revision" = SOURCE_REVISION
  }
}
target "platform-tools" {
  inherits = ["metadata"]
  context = "tools/platform"
}
target "worker" {
  inherits = ["metadata"]
  context = "runtimes/workers"
  tags = [image("worker")]
}
target "execution-manager" {
  inherits = ["metadata"]
  context = "services/execution-manager"
  contexts = { platform-tools = "target:platform-tools" }
  tags = [image("execution-manager")]
}
target "hermes" {
  inherits = ["metadata"]
  context = "."
  dockerfile = "runtimes/hermes/Dockerfile"
  tags = [image("hermes")]
}
target "multica-backend" {
  inherits = ["metadata"]
  context = "services/multica"
  contexts = { multica-source = "vendor/multica" }
  tags = [image("multica-backend")]
}
target "state" {
  inherits = ["metadata"]
  context = "."
  dockerfile = "services/state-tools/Dockerfile"
  tags = [image("state")]
}
