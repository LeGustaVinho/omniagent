import { developmentPipelineCatalog, developmentPipelineName, developmentPipelinePackage } from "@/development-pipeline";

export const workflowTemplates = [{
  key: "development-pipeline",
  name: developmentPipelineName,
  members: developmentPipelineCatalog(),
  createPackage: developmentPipelinePackage,
}];
