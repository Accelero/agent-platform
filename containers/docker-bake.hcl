variable "REGISTRY" { default = "ghcr.io/accelero/agent-platform" }
variable "IMAGE_TAG" { default = "dev" }
variable "SOURCE_REVISION" { default = "" }

function "image" {
  params = [name]
  result = "${REGISTRY}/${name}:${IMAGE_TAG}"
}

group "default" {
  targets = ["hermes", "agent-runtime", "hermes-manager"]
}
target "metadata" {
  labels = {
    "org.opencontainers.image.source" = "https://github.com/Accelero/agent-platform"
    "org.opencontainers.image.revision" = SOURCE_REVISION
  }
}
target "agent-runtime" {
  inherits = ["metadata"]
  context = "runtimes/agent-runtime"
  tags = [image("agent-runtime")]
}
target "hermes" {
  inherits = ["metadata"]
  context = "."
  dockerfile = "runtimes/hermes/Dockerfile"
  tags = [image("hermes")]
}
target "hermes-manager" {
  inherits = ["metadata"]
  context = "."
  dockerfile = "services/hermes-manager/Dockerfile"
  tags = [image("hermes-manager")]
}
