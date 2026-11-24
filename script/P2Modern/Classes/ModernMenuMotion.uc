// No preset writes these rows, and changing one keeps the preset.
class ModernMenuMotion extends ModernMenuPage;

function CreateMenuContents()
{
	Super.CreateMenuContents();

	AddTitle(TitleText, TitleFont, TitleAlign);

	bUpdate = false;
	AddBox(0, "ModernFootPlacement");
	AddLevelSlider(1, "ModernHitReactions", 0, 10, 5);
	AddLevelSlider(2, "ModernCutsceneDof", 0, 10, 4);
	AddLevelSlider(3, "ModernCutsceneMotionBlur", 0, 10, 3);
	AddBack();
	LoadRows();
}

function RowChanged(int i)
{
}

defaultproperties
{
	TitleText="Bodies and Cutscenes"
	BackHelp="Return to the Modern Graphics menu."

	Labels(0)="Foot placement"
	Labels(1)="Hit reactions"
	Labels(2)="Cutscene focus blur"
	Labels(3)="Cutscene motion blur"

	Helps(0)="Feet land on stairs and slopes instead of floating or sinking."
	Helps(1)="How strongly a living person flinches where they are hit. 0 turns it off."
	Helps(2)="Cutscenes blur what is out of focus. 0 turns it off."
	Helps(3)="Cutscenes blur fast movement. 0 turns it off."
}
