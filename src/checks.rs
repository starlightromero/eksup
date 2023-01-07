use super::aws;

use aws_sdk_eks::model::Cluster;
use k8s_openapi::api::core::v1::NodeSystemInfo;
use std::collections::{BTreeMap, HashSet};

pub async fn execute(
  aws_shared_config: &aws_config::SdkConfig,
  cluster: &Cluster,
  nodes: &Vec<NodeSystemInfo>,
) -> Result<(), anyhow::Error> {
  let asg_client = aws_sdk_autoscaling::Client::new(aws_shared_config);
  let ec2_client = aws_sdk_ec2::Client::new(aws_shared_config);
  let eks_client = aws_sdk_eks::Client::new(aws_shared_config);

  version_skew(cluster.version.as_ref().unwrap(), nodes).await?;

  ips_available_for_control_plane(cluster, &ec2_client).await?;

  ips_available_for_data_plane(cluster, &asg_client, &ec2_client, &eks_client).await?;

  Ok(())
}

/// Given a version, parse the minor version
///
/// For example, the format Amazon EKS of v1.20.7-eks-123456 returns 20
/// Or the format of v1.22.7 returns 22
fn parse_minor_version(version: &str) -> Result<u32, anyhow::Error> {
  let version = version.split('.').collect::<Vec<&str>>();
  let minor_version = version[1].parse::<u32>()?;

  Ok(minor_version)
}

/// Given a version, normalize to a consistent format
///
/// For example, the format Amazon EKS uses is v1.20.7-eks-123456 which is normalized to 1.20
fn normalize_version(version: &str) -> Result<String, anyhow::Error> {
  let version = version.split('.').collect::<Vec<&str>>();
  let normalized_version = format!("{}.{}", version[0].replace('v', ""), version[1]);

  Ok(normalized_version)
}

/// Check if there are any nodes that are not at the same minor version as the control plane
///
/// Report on the nodes that do not match the same minor version as the control plane
/// so that users can remediate before upgrading.
///
/// TODO - how to make check results consistent and not one-offs? Needs to align with
/// the goal of multiple return types (JSON, CSV, etc.)
async fn version_skew(
  control_plane_version: &str,
  nodes: &Vec<NodeSystemInfo>,
) -> Result<(), anyhow::Error> {
  let cp_minor = parse_minor_version(control_plane_version)?;
  let mut node_versions: BTreeMap<String, isize> = BTreeMap::new();

  for node in nodes {
    *node_versions
      .entry(node.kubelet_version.clone())
      .or_insert(0) += 1;
  }

  for (key, value) in node_versions.iter() {
    let minor = parse_minor_version(key)?;
    if minor != cp_minor {
      let version = normalize_version(key)?;
      println!("There are {value} nodes that are at version v{version} which do not match the control plane version v{control_plane_version}");
    }
  }

  Ok(())
}

/// Check if there are enough IPs available for the control plane to use (> 5 IPs)
async fn ips_available_for_control_plane(
  cluster: &Cluster,
  client: &aws_sdk_ec2::Client,
) -> Result<(), anyhow::Error> {
  let subnet_ids = cluster
    .resources_vpc_config()
    .unwrap()
    .subnet_ids
    .as_ref()
    .unwrap();

  let subnets = aws::get_subnets(client, subnet_ids.clone()).await?;

  let available_ips: i32 = subnets
    .iter()
    .map(|subnet| subnet.available_ip_address_count.unwrap())
    .sum();

  println!("There are {available_ips:#?} available IPs for the control plane to use");

  Ok(())
}

async fn ips_available_for_data_plane(
  cluster: &Cluster,
  asg_client: &aws_sdk_autoscaling::Client,
  ec2_client: &aws_sdk_ec2::Client,
  eks_client: &aws_sdk_eks::Client,
) -> Result<(), anyhow::Error> {
  let mut subnet_ids = HashSet::new();

  // EKS managed node group subnets
  let eks_mngs =
    aws::get_eks_managed_node_groups(eks_client, cluster.name.as_ref().unwrap()).await?;
  if let Some(nodegroups) = eks_mngs {
    for group in nodegroups {
      let subnets = group.subnets.unwrap();
      for subnet in subnets {
        subnet_ids.insert(subnet);
      }
    }
  }

  // Self managed node group subnets
  let self_mngs =
    aws::get_self_managed_node_groups(asg_client, cluster.name.as_ref().unwrap()).await?;
  if let Some(nodegroups) = self_mngs {
    for group in nodegroups {
      let subnets = group.vpc_zone_identifier.unwrap();
      for subnet in subnets.split(',') {
        subnet_ids.insert(subnet.to_string());
      }
    }
  }

  // Fargate profile subnets
  let fargate_profiles =
    aws::get_fargate_profiles(eks_client, cluster.name.as_ref().unwrap()).await?;
  if let Some(profiles) = fargate_profiles {
    for profile in profiles {
      let subnets = profile.subnets.unwrap();
      for subnet in subnets {
        subnet_ids.insert(subnet);
      }
    }
  }

  let subnets = aws::get_subnets(ec2_client, subnet_ids.into_iter().collect()).await?;

  let available_ips: i32 = subnets
    .iter()
    .map(|subnet| subnet.available_ip_address_count.unwrap())
    .sum();

  println!("There are {available_ips:#?} available IPs for the data plane to use");

  Ok(())
}
