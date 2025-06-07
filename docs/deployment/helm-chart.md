# Helm Chart Deployment

This guide explains how to deploy the Image Resize Service to Kubernetes using the provided Helm chart.

## Prerequisites

- Kubernetes cluster
- Helm 3 installed

## Chart Location

The Helm chart is located in the `helm/emgr/` directory in the repository.

## Configuration

You can customize the deployment by modifying the `helm/emgr/values.yaml` file.

Key configuration options:

- `replicaCount`: Number of replicas
- `image.repository`: Docker image repository
- `image.pullPolicy`: Image pull policy
- `image.tag`: Image tag
- `service.type`: Service type (ClusterIP, NodePort, LoadBalancer)
- `service.port`: Service port
- `ingress.enabled`: Enable Ingress
- `ingress.className`: Ingress class name
- `ingress.hosts`: Ingress hosts
- `ingress.tls`: Ingress TLS configuration
- `resources`: CPU/memory resource requests and limits
- `autoscaling.enabled`: Enable Horizontal Pod Autoscaler
- `autoscaling.minReplicas`: Minimum replicas for HPA
- `autoscaling.maxReplicas`: Maximum replicas for HPA
- `autoscaling.targetCPUUtilizationPercentage`: Target CPU utilization
- `autoscaling.targetMemoryUtilizationPercentage`: Target memory utilization
- `envVars`: Environment variables for the application (see [Configuration](../getting-started/configuration.md))

## Deployment Steps

### 1. Add Helm Repository (if applicable)

If the chart is hosted in a Helm repository, add it first:

```bash
helm repo add <repo-name> <repo-url>
helm repo update
```

### 2. Install the Chart

Navigate to the chart directory or use the repository:

```bash
# From local directory
helm install image-resize ./helm/emgr --namespace image-resize --create-namespace

# Or from Helm repository
helm install image-resize <repo-name>/emgr --namespace image-resize --create-namespace
```

### 3. Verify the Deployment

```bash
kubectl get pods -n image-resize
kubectl get svc -n image-resize
```

## Upgrading the Deployment

```bash
helm upgrade image-resize ./helm/emgr --namespace image-resize
```

## Uninstalling the Deployment

```bash
helm uninstall image-resize --namespace image-resize
```

## GitHub Pages Deployment

The documentation itself can be deployed to GitHub Pages. This is typically handled by a GitHub Actions workflow. See [GitHub Actions Workflow](#github-actions-workflow) for more details.